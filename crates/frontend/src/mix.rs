//! The console's sound as samples. The sound chips play the writes the game made at the
//! master clocks it made them, and each YM2612 output period, [`SAMPLE_CLOCKS`], becomes one
//! stereo sample at [`SAMPLE_RATE`]: the YM2612's output for that period, which sees every
//! write made before the period ends (as the chip computes a period's output once), plus
//! the PSG averaged over the period (the area under its output, so a level that lasts part
//! of a period counts for that part). A high-pass at [`HIGH_PASS_HZ`] then takes out the
//! constant the YM2612's DAC adds even in silence, as the console's output capacitors do.
//!
//! The chips sit outside the emulation: they only take the writes the run has made, and
//! nothing in the run reads them, so sound cannot change what the run does.

use anygenesis_psg::Psg;
use anygenesis_runtime::{Chip, SoundWrite, timing::MASTER_CLOCK_HZ};
use anygenesis_ym2612::{CHANNELS as YM_CHANNELS, Output, Ym2612};

use crate::audio::CHANNELS;

/// Master clocks per output sample: 144 YM2612 clocks of 7 master clocks each.
pub const SAMPLE_CLOCKS: u64 = anygenesis_ym2612::SAMPLE_CLOCKS;

/// Output samples per second: 53,693,175 / 1008 = 53,267.04, rounded.
pub const SAMPLE_RATE: i32 = (MASTER_CLOCK_HZ / SAMPLE_CLOCKS) as i32;

/// Master clocks per PSG step: the PSG's clock is the master clock divided by 15.
const PSG_STEP_CLOCKS: u64 = 15 * anygenesis_psg::STEP_CLOCKS;

/// The loudest a YM2612 channel comes out of its DAC: a 9-bit level of 256 and the DAC's
/// offset of 4 (see [`Output::sides`]).
const YM_LOUDEST: f32 = 260.0;

/// Full scale per YM2612 level unit: all six channels at their loudest reach full scale.
const YM_GAIN: f32 = 1.0 / (YM_CHANNELS as f32 * YM_LOUDEST);

/// Full scale per PSG level unit: all four channels at their loudest match one YM2612
/// channel at its loudest. The official balance is not public; hardware owners put the PSG
/// at about 1/6 of an emulator's YM2612 (research note v0.2-m3.3-research-psg.md), to be
/// tuned by ear.
const PSG_GAIN: f32 = 1.0 / (4.0 * YM_CHANNELS as f32);

/// The output high-pass's corner. The console's own is not documented; this one only has to
/// remove a constant and stay below anything audible [U].
const HIGH_PASS_HZ: f64 = 5.0;

/// The one-pole high-pass's feedback for [`HIGH_PASS_HZ`] at [`SAMPLE_RATE`]: e^(-2 pi f /
/// rate).
fn high_pass_pole() -> f32 {
    (-2.0 * std::f64::consts::PI * HIGH_PASS_HZ / f64::from(SAMPLE_RATE)).exp() as f32
}

/// The sound chips and how far they have played.
#[derive(Debug, Clone)]
pub struct Mixer {
    psg: Psg,
    ym: Ym2612,
    /// The master clock played to.
    clock: u64,
    /// The PSG's output since the current sample began, times master clocks.
    area: f64,
    high_pass: HighPass,
}

impl Default for Mixer {
    fn default() -> Self {
        let ym = Ym2612::default();
        // The high-pass starts settled on the chips' silence, so sound starts without a step.
        let silence = ym_sides(&Output::default());
        Self {
            psg: Psg::default(),
            ym,
            clock: 0,
            area: 0.0,
            high_pass: HighPass::settled(silence),
        }
    }
}

/// A one-pole high-pass per side: `y = x - x' + pole * y'`.
#[derive(Debug, Clone, Copy)]
struct HighPass {
    pole: f32,
    input: [f32; CHANNELS],
    output: [f32; CHANNELS],
}

impl HighPass {
    /// A high-pass that has had `level` on each side for ever.
    fn settled(level: [f32; CHANNELS]) -> Self {
        Self {
            pole: high_pass_pole(),
            input: level,
            output: [0.0; CHANNELS],
        }
    }

    fn filter(&mut self, sample: [f32; CHANNELS]) -> [f32; CHANNELS] {
        let pole = self.pole;
        for ((output, input), sample) in self.output.iter_mut().zip(&mut self.input).zip(sample) {
            *output = sample - *input + pole * *output;
            *input = sample;
        }
        self.output
    }
}

impl Mixer {
    /// Plays `writes`, in time order and none before what was played already, and goes on
    /// to master clock `until`, adding every sample that ends by then to `out` as
    /// interleaved stereo.
    pub fn play(&mut self, writes: &[SoundWrite], until: u64, out: &mut Vec<f32>) {
        for write in writes {
            self.run(write.clock, out);
            match write.chip {
                Chip::Psg => self.psg.write(write.value),
                Chip::Ym2612 { part, register } => self.ym.write(part, register, write.value),
                Chip::Ym2612Reset => self.ym.reset(),
            }
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
                let psg = (self.area / SAMPLE_CLOCKS as f64) as f32 * PSG_GAIN;
                let ym = ym_sides(&self.ym.sample());
                out.extend(self.high_pass.filter(ym.map(|side| side + psg)));
                self.area = 0.0;
            }
            if end == step {
                self.psg.step();
            }
        }
    }
}

/// The YM2612's channels out of its DAC on each side, scaled to full scale.
fn ym_sides(output: &Output) -> [f32; CHANNELS] {
    output.sides().map(|sum| sum as f32 * YM_GAIN)
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
        let silence = ym_sides(&Output::default());
        let raw = [0.0, 0.75 * full, full].map(|psg| silence.map(|side| side + psg));
        assert_eq!(out, through_high_pass(&raw));
        assert_eq!(out[2], out[3], "both channels");
    }

    /// `raw` stereo samples through a fresh mixer's high-pass.
    fn through_high_pass(raw: &[[f32; CHANNELS]]) -> Vec<f32> {
        let mut high_pass = Mixer::default().high_pass;
        raw.iter()
            .flat_map(|&sample| high_pass.filter(sample))
            .collect()
    }

    /// The YM2612's sides with only channel 6 playing, at `levels`.
    fn channel_6(levels: [i16; CHANNELS]) -> [f32; CHANNELS] {
        let mut output = Output::default();
        output.channels[YM_CHANNELS - 1] = levels;
        ym_sides(&output)
    }

    fn ym(clock: u64, part: u8, register: u8, value: u8) -> SoundWrite {
        SoundWrite {
            clock,
            cpu: Cpu::Z80,
            chip: Chip::Ym2612 { part, register },
            value,
        }
    }

    #[test]
    fn test_play_holds_the_dac_level_a_period_sees_at_its_end() {
        let mut mixer = Mixer::default();
        let writes = [
            ym(0, 1, 0x2B, 0x80),
            ym(10, 1, 0x2A, 0xFF),
            ym(SAMPLE_CLOCKS - 1, 1, 0x2A, 0xC0),
            ym(SAMPLE_CLOCKS, 1, 0x2A, 0x40),
            ym(SAMPLE_CLOCKS + 1, 2, 0xB6, 0x80),
        ];
        let mut out = Vec::new();
        mixer.play(&writes, SAMPLE_CLOCKS * 3, &mut out);
        let raw = [[128, 128], [-128, 0], [-128, 0]].map(channel_6);
        assert_eq!(out, through_high_pass(&raw));
    }

    #[test]
    fn test_play_resets_the_ym2612_with_the_reset_line() {
        let mut mixer = Mixer::default();
        let mut reset = ym(SAMPLE_CLOCKS, 1, 0, 0);
        reset.chip = Chip::Ym2612Reset;
        let writes = [ym(0, 1, 0x2B, 0x80), ym(0, 1, 0x2A, 0xFF), reset];
        let mut out = Vec::new();
        mixer.play(&writes, SAMPLE_CLOCKS * 2, &mut out);
        let raw = [channel_6([254, 254]), ym_sides(&Output::default())];
        assert_eq!(out, through_high_pass(&raw));
    }

    #[test]
    fn test_high_pass_takes_a_constant_out_and_passes_a_step() {
        let mut high_pass = HighPass::settled([0.0; CHANNELS]);
        assert_eq!(high_pass.filter([0.5, -0.5]), [0.5, -0.5]);
        let settled = (0..SAMPLE_RATE).fold([0.0; CHANNELS], |_, _| high_pass.filter([0.5, -0.5]));
        assert!(settled.iter().all(|side| side.abs() < 1e-6), "{settled:?}");
    }

    #[test]
    fn test_gains_put_six_ym2612_channels_at_full_scale_and_the_psg_at_one() {
        assert_eq!(YM_GAIN * YM_LOUDEST * YM_CHANNELS as f32, 1.0);
        assert_eq!(PSG_GAIN * 4.0, YM_GAIN * YM_LOUDEST);
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
