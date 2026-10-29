//! The console's sound as samples. The sound chips play the writes the game made at the
//! master clocks it made them, and their output is averaged over each YM2612 output period,
//! [`SAMPLE_CLOCKS`], into one stereo sample at [`SAMPLE_RATE`]: the area under the chips'
//! output, so a level that lasts part of a period counts for that part. The PSG is the only
//! chip so far; the YM2612 makes one sample per period of its own and joins sample for
//! sample.
//!
//! The chips sit outside the emulation: they only take the writes the run has made, and
//! nothing in the run reads them, so sound cannot change what the run does.

use anygenesis_psg::Psg;
use anygenesis_runtime::{Chip, SoundWrite, timing::MASTER_CLOCK_HZ};

use crate::audio::CHANNELS;

/// Master clocks per output sample: 144 YM2612 clocks of 7 master clocks each.
pub const SAMPLE_CLOCKS: u64 = 1008;

/// Output samples per second: 53,693,175 / 1008 = 53,267.04, rounded.
pub const SAMPLE_RATE: i32 = (MASTER_CLOCK_HZ / SAMPLE_CLOCKS) as i32;

/// Master clocks per PSG step: the PSG's clock is the master clock divided by 15.
const PSG_STEP_CLOCKS: u64 = 15 * anygenesis_psg::STEP_CLOCKS;

/// Full scale per PSG level unit: all four channels at their loudest reach half of full
/// scale. The balance against the YM2612 is set when it plays (hardware owners put the PSG
/// at about 1/6 of an emulator's YM2612 channel, research note v0.2-m3.3-research-psg.md).
const PSG_GAIN: f32 = 0.125;

/// The sound chips and how far they have played.
#[derive(Debug, Clone, Default)]
pub struct Mixer {
    psg: Psg,
    /// The master clock played to.
    clock: u64,
    /// The output since the current sample began, times master clocks.
    area: f64,
}

impl Mixer {
    /// Plays `writes`, in time order and none before what was played already, and goes on
    /// to master clock `until`, adding every sample that ends by then to `out` as
    /// interleaved stereo.
    pub fn play(&mut self, writes: &[SoundWrite], until: u64, out: &mut Vec<f32>) {
        for write in writes.iter().filter(|write| write.chip == Chip::Psg) {
            self.run(write.clock, out);
            self.psg.write(write.value);
        }
        self.run(until, out);
    }

    /// Runs the chips to master clock `until`.
    fn run(&mut self, until: u64, out: &mut Vec<f32>) {
        while self.clock < until {
            let step = next_multiple(self.clock, PSG_STEP_CLOCKS);
            let sample = next_multiple(self.clock, SAMPLE_CLOCKS);
            let end = step.min(sample).min(until);
            self.area += f64::from(self.psg.output()) * (end - self.clock) as f64;
            self.clock = end;
            if end == sample {
                let value = (self.area / SAMPLE_CLOCKS as f64) as f32 * PSG_GAIN;
                out.extend([value; CHANNELS]);
                self.area = 0.0;
            }
            if end == step {
                self.psg.step();
            }
        }
    }
}

/// The first multiple of `period` after `clock`.
fn next_multiple(clock: u64, period: u64) -> u64 {
    (clock / period + 1) * period
}

/// Interleaved stereo samples as a WAV file: 16-bit PCM at [`SAMPLE_RATE`].
pub fn wav(samples: &[f32]) -> Vec<u8> {
    const HEADER: usize = 44;
    const BYTES: u16 = 2;
    let channels = CHANNELS as u16;
    let data = (samples.len() * usize::from(BYTES)) as u32;
    let mut file = Vec::with_capacity(HEADER + data as usize);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(data + HEADER as u32 - 8).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&channels.to_le_bytes());
    file.extend_from_slice(&(SAMPLE_RATE as u32).to_le_bytes());
    let frame = channels * BYTES;
    file.extend_from_slice(&(SAMPLE_RATE as u32 * u32::from(frame)).to_le_bytes());
    file.extend_from_slice(&frame.to_le_bytes());
    file.extend_from_slice(&(BYTES * 8).to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        file.extend_from_slice(&value.to_le_bytes());
    }
    file
}

#[cfg(test)]
mod tests {
    use anygenesis_runtime::Cpu;

    use super::*;

    fn psg(clock: u64, value: u8) -> SoundWrite {
        SoundWrite {
            clock,
            cpu: Cpu::M68000,
            chip: Chip::Psg,
            value,
        }
    }

    #[test]
    fn test_sample_rate_is_the_ym2612_output_rate() {
        assert_eq!(SAMPLE_RATE, 53_267);
        assert_eq!(PSG_STEP_CLOCKS, 240);
    }

    #[test]
    fn test_play_makes_one_stereo_sample_per_period_carrying_the_rest() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.play(&[], SAMPLE_CLOCKS * 3 + 500, &mut out);
        assert_eq!(out.len(), 3 * CHANNELS);
        mixer.play(&[], SAMPLE_CLOCKS * 4, &mut out);
        assert_eq!(out.len(), 4 * CHANNELS);
    }

    #[test]
    fn test_play_is_silent_at_power_on_with_every_channel_off() {
        let mut mixer = Mixer::default();
        for channel in 0..4u8 {
            mixer.psg.write(0x9F | channel << 5);
        }
        let mut out = Vec::new();
        mixer.play(&[], SAMPLE_CLOCKS * 100, &mut out);
        assert!(out.iter().all(|&value| value == 0.0));
    }

    #[test]
    fn test_play_averages_a_level_over_the_part_of_the_period_it_lasts() {
        let mut mixer = Mixer::default();
        for channel in 0..4u8 {
            mixer.psg.write(0x9F | channel << 5);
        }
        let mut out = Vec::new();
        mixer.play(&[], SAMPLE_CLOCKS, &mut out);
        // Tone 0 at its longest period holds its level for over 200,000 master clocks once it
        // reloads (at the first step, before the volume write); at full volume from a quarter
        // of the second sample.
        let writes = [psg(SAMPLE_CLOCKS, 0x8F), psg(SAMPLE_CLOCKS, 0x3F)];
        mixer.play(&writes, SAMPLE_CLOCKS + 252, &mut out);
        mixer.play(
            &[psg(SAMPLE_CLOCKS + 252, 0x90)],
            SAMPLE_CLOCKS * 3,
            &mut out,
        );
        let full = mixer.psg.output() * PSG_GAIN;
        assert_ne!(full, 0.0);
        assert!((out[2] - full * 0.75).abs() < 1e-6, "{} vs {full}", out[2]);
        assert!((out[4] - full).abs() < 1e-6, "{} vs {full}", out[4]);
        assert_eq!(out[2], out[3], "both channels");
    }

    #[test]
    fn test_play_ignores_ym2612_writes() {
        let write = SoundWrite {
            chip: Chip::Ym2612 {
                part: 1,
                register: 0x2A,
            },
            ..psg(10, 0x90)
        };
        let (mut with, mut without) = (Mixer::default(), Mixer::default());
        let (mut heard, mut silent) = (Vec::new(), Vec::new());
        with.play(&[write], SAMPLE_CLOCKS, &mut heard);
        without.play(&[], SAMPLE_CLOCKS, &mut silent);
        assert_eq!(with.psg, without.psg);
        assert_eq!(heard, silent);
    }

    #[test]
    fn test_wav_header_describes_16_bit_stereo_at_the_sample_rate() {
        let file = wav(&[0.0, 1.0, -1.0, 0.5]);
        assert_eq!(file.len(), 44 + 8);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(file[4..8].try_into().expect("4 bytes")),
            44
        );
        assert_eq!(&file[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([file[22], file[23]]), 2);
        assert_eq!(
            u32::from_le_bytes(file[24..28].try_into().expect("4 bytes")),
            53_267
        );
        assert_eq!(u16::from_le_bytes([file[34], file[35]]), 16);
        assert_eq!(
            u32::from_le_bytes(file[40..44].try_into().expect("4 bytes")),
            8
        );
        let values: Vec<i16> = file[44..]
            .chunks(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        assert_eq!(values, [0, 32_767, -32_767, 16_384]);
    }
}
