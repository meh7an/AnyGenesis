//! Sound out of the window. The run thread pushes each frame's samples, stereo at the
//! YM2612's output rate ([`SAMPLE_RATE`]), into a lock-free ring; SDL's audio thread pulls
//! them in its stream callback and resamples them to the device.
//!
//! The run thread keeps its own clock ([`crate::pace`]) with or without sound, and the
//! device's clock is never quite the same (77 ppm apart on the dev host at M0.4), so the
//! callback steers SDL's frequency ratio from how full the ring is, within
//! [`MAX_DEVIATION`], and the ring neither drains nor floods. A new epoch of the schedule (a
//! pause, a reset, a re-anchor) empties the ring, and sound waits for a fresh prefill; so
//! does a ring that ran dry. Sound is optional: without a device the game runs silent.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use sdl3::{
    AudioSubsystem,
    audio::{AudioCallback, AudioFormat, AudioSpec, AudioStream, AudioStreamWithCallback},
};

use crate::mix::SAMPLE_RATE;

/// Interleaved channels: left, right.
pub const CHANNELS: usize = 2;

/// Ring capacity in sample values: 8,192 stereo frames, about 154 ms.
const RING_VALUES: usize = 16_384;

/// The fill, in values, sound waits for before it starts and the rate control steers to:
/// 2,048 stereo frames, about 38 ms (no underruns over the M0.4 runs).
const TARGET_VALUES: usize = 2_048 * CHANNELS;

/// Largest deviation of the frequency ratio from 1.
const MAX_DEVIATION: f64 = 0.005;

/// Weight of each new fill reading in the smoothed fill.
const SMOOTHING: f64 = 0.02;

/// A single-producer, single-consumer ring of f32 sample values. Positions count every
/// value ever written or read; at 106,534 values a second a 64-bit count never wraps.
#[derive(Debug)]
struct Ring {
    slots: Box<[AtomicU32]>,
    written: AtomicU64,
    read: AtomicU64,
}

impl Ring {
    fn new() -> Self {
        Self {
            slots: (0..RING_VALUES).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
        }
    }

    fn slot(&self, position: u64) -> &AtomicU32 {
        &self.slots[(position % RING_VALUES as u64) as usize]
    }

    /// Values waiting to be read.
    fn len(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        (self.written.load(Ordering::Acquire) - read) as usize
    }

    /// Appends as many whole stereo frames of `values` as fit; returns how many values did.
    /// The producer's side only.
    fn push(&self, values: &[f32]) -> usize {
        let written = self.written.load(Ordering::Relaxed);
        let free = RING_VALUES - (written - self.read.load(Ordering::Acquire)) as usize;
        let count = values.len().min(free) & !(CHANNELS - 1);
        for (position, value) in (written..).zip(&values[..count]) {
            self.slot(position)
                .store(value.to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(written + count as u64, Ordering::Release);
        count
    }

    /// Moves up to `count` values, whole stereo frames, to the end of `out`. The
    /// consumer's side only.
    fn pop(&self, count: usize, out: &mut Vec<f32>) {
        let read = self.read.load(Ordering::Relaxed);
        let available = (self.written.load(Ordering::Acquire) - read) as usize;
        let count = count.min(available) & !(CHANNELS - 1);
        let end = read + count as u64;
        out.extend(
            (read..end).map(|position| f32::from_bits(self.slot(position).load(Ordering::Relaxed))),
        );
        self.read.store(end, Ordering::Release);
    }

    /// Drops every value before `position`, if not read yet. The consumer's side only.
    fn skip_to(&self, position: u64) {
        if position > self.read.load(Ordering::Relaxed) {
            self.read.store(position, Ordering::Release);
        }
    }
}

/// What the run thread, the window and SDL's audio thread share of the sound.
///
/// Only one thread may push (the run thread); the window sets mute and hold.
#[derive(Debug)]
pub struct Sound {
    ring: Ring,
    /// Where the current epoch's samples start in the ring; everything before goes unplayed.
    epoch_start: AtomicU64,
    muted: AtomicBool,
    /// The run is held, so a ring running dry is expected, not an underrun.
    held: AtomicBool,
    underruns: AtomicU64,
    /// Values the run made that found the ring full.
    dropped: AtomicU64,
}

impl Sound {
    pub fn new(muted: bool) -> Self {
        Self {
            ring: Ring::new(),
            epoch_start: AtomicU64::new(0),
            muted: AtomicBool::new(muted),
            held: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Queues interleaved stereo samples to play after those queued before.
    pub fn push(&self, values: &[f32]) {
        let pushed = self.ring.push(values);
        self.dropped
            .fetch_add((values.len() - pushed) as u64, Ordering::Relaxed);
    }

    /// Starts a new epoch: what is queued now goes unplayed, and sound waits for a prefill of
    /// what is pushed next.
    pub fn restart(&self) {
        let written = self.ring.written.load(Ordering::Relaxed);
        self.epoch_start.store(written, Ordering::Release);
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_held(&self, held: bool) {
        self.held.store(held, Ordering::Relaxed);
    }

    /// Times the ring ran dry while the run was not held.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Values dropped because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// SDL's side: the stream callback.
#[derive(Debug)]
pub struct Output {
    sound: Arc<Sound>,
    /// The epoch start last seen.
    epoch_start: u64,
    /// Whether the prefill was reached and samples are being played.
    playing: bool,
    /// The ring's fill, smoothed.
    smoothed: f64,
    buffer: Vec<f32>,
}

impl Output {
    fn new(sound: Arc<Sound>) -> Self {
        Self {
            sound,
            epoch_start: 0,
            playing: false,
            smoothed: 0.0,
            buffer: Vec::new(),
        }
    }

    /// Fills `buffer` with `requested` values to play and says what frequency ratio to set,
    /// if any.
    fn fill(&mut self, requested: usize) -> Option<f32> {
        let sound = &*self.sound;
        let epoch_start = sound.epoch_start.load(Ordering::Acquire);
        if epoch_start != self.epoch_start {
            self.epoch_start = epoch_start;
            sound.ring.skip_to(epoch_start);
            self.playing = false;
        }
        let fill = sound.ring.len();
        if !self.playing && fill >= TARGET_VALUES {
            self.playing = true;
            self.smoothed = fill as f64;
        }
        self.buffer.clear();
        if self.playing {
            sound.ring.pop(requested, &mut self.buffer);
            if self.buffer.len() < requested {
                if !sound.held.load(Ordering::Relaxed) {
                    sound.underruns.fetch_add(1, Ordering::Relaxed);
                }
                self.playing = false;
            }
        }
        if sound.muted.load(Ordering::Relaxed) {
            self.buffer.clear();
        }
        self.buffer.resize(requested, 0.0);
        self.playing.then(|| {
            self.smoothed += SMOOTHING * (fill as f64 - self.smoothed);
            let target = TARGET_VALUES as f64;
            let error = ((self.smoothed - target) / target).clamp(-1.0, 1.0);
            (1.0 + MAX_DEVIATION * error) as f32
        })
    }
}

impl AudioCallback<f32> for Output {
    fn callback(&mut self, stream: &mut AudioStream, requested: i32) {
        let ratio = self.fill(usize::try_from(requested).unwrap_or(0));
        // A failed put costs this buffer only; SDL plays silence for it.
        let _ = stream.put_data_f32(&self.buffer);
        if let Some(ratio) = ratio {
            // SAFETY: the stream pointer is live for the whole callback, and SDL allows the
            // ratio to be set from any thread (it takes the stream's own lock). sdl3 0.20.0
            // has no wrapper for it.
            unsafe { sdl3::sys::audio::SDL_SetAudioStreamFrequencyRatio(stream.raw(), ratio) };
        }
    }
}

/// Opens the default playback device with a callback stream at [`SAMPLE_RATE`] stereo f32
/// playing `sound`. Never fatal: on failure it says why and the game runs silent.
pub fn open(audio: &AudioSubsystem, sound: &Arc<Sound>) -> Option<AudioStreamWithCallback<Output>> {
    let spec = AudioSpec::new(
        Some(SAMPLE_RATE),
        Some(CHANNELS as i32),
        Some(AudioFormat::f32_sys()),
    );
    let stream = audio
        .open_playback_stream(&spec, Output::new(Arc::clone(sound)))
        .inspect_err(|error| eprintln!("sound: no stream, running silent: {error}"))
        .ok()?;
    stream
        .resume()
        .inspect_err(|error| eprintln!("sound: the stream did not start, running silent: {error}"))
        .ok()?;
    Some(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(count: usize, value: f32) -> Vec<f32> {
        vec![value; count * CHANNELS]
    }

    #[test]
    fn test_ring_push_stops_at_capacity_in_whole_frames() {
        let ring = Ring::new();
        assert_eq!(
            ring.push(&frames(RING_VALUES / CHANNELS - 1, 1.0)),
            RING_VALUES - 2
        );
        assert_eq!(ring.push(&[2.0, 2.0, 3.0, 3.0]), 2);
        assert_eq!(ring.len(), RING_VALUES);
        assert_eq!(ring.push(&[4.0, 4.0]), 0);
    }

    #[test]
    fn test_ring_pop_returns_values_in_order_across_the_wrap() {
        let ring = Ring::new();
        let mut out = Vec::new();
        ring.push(&frames(RING_VALUES / CHANNELS - 1, 0.0));
        ring.pop(RING_VALUES - 2, &mut out);
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        out.clear();
        ring.pop(8, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn test_ring_pop_takes_whole_frames_only() {
        let ring = Ring::new();
        let mut out = Vec::new();
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        ring.pop(3, &mut out);
        assert_eq!(out, [1.0, 2.0]);
    }

    #[test]
    fn test_output_waits_for_the_prefill_then_plays() {
        let sound = Arc::new(Sound::new(false));
        let mut output = Output::new(Arc::clone(&sound));
        sound.push(&frames(TARGET_VALUES / CHANNELS - 1, 0.5));
        assert_eq!(output.fill(4), None);
        assert_eq!(output.buffer, [0.0; 4]);
        sound.push(&frames(1, 0.5));
        assert!(output.fill(4).is_some());
        assert_eq!(output.buffer, [0.5; 4]);
    }

    #[test]
    fn test_output_ratio_follows_the_fill() {
        let sound = Arc::new(Sound::new(false));
        let mut output = Output::new(Arc::clone(&sound));
        sound.push(&frames(TARGET_VALUES / CHANNELS, 0.0));
        let ratio = output.fill(2).expect("plays");
        assert!((ratio - 1.0).abs() < 1e-6, "{ratio}");
        sound.push(&frames(RING_VALUES / CHANNELS, 0.0));
        output.smoothed = RING_VALUES as f64;
        let ratio = output.fill(2).expect("plays");
        assert!(
            (ratio - (1.0 + MAX_DEVIATION as f32)).abs() < 1e-6,
            "{ratio}"
        );
    }

    #[test]
    fn test_output_running_dry_counts_an_underrun_and_waits_again() {
        let sound = Arc::new(Sound::new(false));
        let mut output = Output::new(Arc::clone(&sound));
        sound.push(&frames(TARGET_VALUES / CHANNELS, 0.5));
        output.fill(TARGET_VALUES + 2);
        assert_eq!(sound.underruns(), 1);
        assert_eq!(output.buffer[TARGET_VALUES..], [0.0, 0.0]);
        sound.push(&frames(1, 0.5));
        assert_eq!(output.fill(2), None, "waits for a new prefill");
    }

    #[test]
    fn test_output_running_dry_while_held_is_no_underrun() {
        let sound = Arc::new(Sound::new(false));
        let mut output = Output::new(Arc::clone(&sound));
        sound.push(&frames(TARGET_VALUES / CHANNELS, 0.5));
        sound.set_held(true);
        output.fill(TARGET_VALUES + 2);
        assert_eq!(sound.underruns(), 0);
    }

    #[test]
    fn test_restart_drops_what_was_queued_before() {
        let sound = Arc::new(Sound::new(false));
        let mut output = Output::new(Arc::clone(&sound));
        sound.push(&frames(TARGET_VALUES / CHANNELS, 0.5));
        sound.restart();
        sound.push(&frames(TARGET_VALUES / CHANNELS, 0.25));
        assert!(output.fill(2).is_some());
        assert_eq!(output.buffer, [0.25, 0.25]);
        assert_eq!(sound.ring.len(), TARGET_VALUES - 2);
    }

    #[test]
    fn test_muted_output_plays_silence_and_keeps_taking_samples() {
        let sound = Arc::new(Sound::new(true));
        let mut output = Output::new(Arc::clone(&sound));
        sound.push(&frames(TARGET_VALUES / CHANNELS, 0.5));
        assert!(output.fill(4).is_some());
        assert_eq!(output.buffer, [0.0; 4]);
        assert_eq!(sound.ring.len(), TARGET_VALUES - 4);
    }

    #[test]
    fn test_push_counts_values_dropped_on_a_full_ring() {
        let sound = Sound::new(false);
        sound.push(&frames(RING_VALUES / CHANNELS + 3, 0.0));
        assert_eq!(sound.dropped(), 6);
    }
}
