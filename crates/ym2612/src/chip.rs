//! The chip's sound: its register file as the game wrote it, and one stereo output per
//! output period. It plays outside the emulation, from the writes the game made, so nothing
//! in a run depends on it.
//!
//! FM synthesis is not built yet, so channels 1-6 put out 0; the DAC, when on, takes
//! channel 6's place.
//!
//! Sources: $2A an unsigned 8-bit sample, $2B bit 7 the DAC in place of channel 6
//! (Plutiedev, md.railgun.works); $2C bit 3 the DAC's ninth, lowest bit (Sauraen on
//! SpritesMind, p=26996); channel levels 9-bit signed with silence at 0 (Sauraen, t=386
//! start=795), one per output period of 144 input clocks (Sauraen, p=27124). Inferred [U]:
//! the level ((sample - $80) << 1) | bit 3, that the DAC follows channel 6's panning, and
//! that a side whose panning bit is off puts out 0. Power-on: every register 0 but the
//! panning registers, both sides on (Snake and Nemesis, t=386, p=7999 and p=8020); that the
//! reset line does the same is assumed [U] (Snake recalls it not clearing things). ymfm
//! starts its DAC at the centre rather than at sample 0.

use crate::registers::{DAC_DATA, DAC_ENABLE, PAN, TEST};

/// FM channels: three per part.
pub const CHANNELS: usize = 6;

/// Register $2B's bit that turns the DAC on.
const DAC_ON: u8 = 0x80;
/// Register $2C's bit that is the DAC's lowest bit.
const DAC_LOW_BIT: u8 = 0x08;
/// A panning register's left and right bits.
const LEFT: u8 = 0x80;
const RIGHT: u8 = 0x40;
/// The panning registers after a reset: both sides on.
const PAN_RESET: u8 = LEFT | RIGHT;

/// One output period: each channel's 9-bit signed level on the left and right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Output {
    pub channels: [[i16; 2]; CHANNELS],
}

/// The register file of both parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ym2612 {
    registers: [[u8; 256]; 2],
}

impl Default for Ym2612 {
    fn default() -> Self {
        let mut chip = Self {
            registers: [[0; 256]; 2],
        };
        chip.reset();
        chip
    }
}

impl Ym2612 {
    /// Every register back to its power-on value.
    pub fn reset(&mut self) {
        self.registers = [[0; 256]; 2];
        for part in &mut self.registers {
            part[usize::from(PAN)..usize::from(PAN) + 3].fill(PAN_RESET);
        }
    }

    /// A data write to `register` of `part` (1 or 2).
    pub fn write(&mut self, part: u8, register: u8, value: u8) {
        self.registers[usize::from(part - 1)][usize::from(register)] = value;
    }

    /// The output of the next period.
    pub fn sample(&mut self) -> Output {
        let mut output = Output::default();
        if self.register(1, DAC_ENABLE) & DAC_ON != 0 {
            let dac = self.dac();
            let pan = self.register(2, PAN + 2);
            output.channels[CHANNELS - 1] = [side(pan, LEFT, dac), side(pan, RIGHT, dac)];
        }
        output
    }

    /// The DAC's 9-bit signed level: the sample, offset so $80 is 0, above the low bit.
    fn dac(&self) -> i16 {
        let high = u16::from(self.register(1, DAC_DATA) ^ 0x80) << 1;
        let low = u16::from(self.register(1, TEST) & DAC_LOW_BIT != 0);
        ((high | low) << 7) as i16 >> 7
    }

    fn register(&self, part: u8, register: u8) -> u8 {
        self.registers[usize::from(part - 1)][usize::from(register)]
    }
}

/// A channel's `level` on the side whose panning bit is `bit`.
fn side(pan: u8, bit: u8, level: i16) -> i16 {
    if pan & bit != 0 { level } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dac(sample: u8, low: bool) -> Output {
        let mut chip = Ym2612::default();
        chip.write(1, DAC_ENABLE, DAC_ON);
        chip.write(1, DAC_DATA, sample);
        chip.write(1, TEST, if low { DAC_LOW_BIT } else { 0 });
        chip.sample()
    }

    #[test]
    fn test_dac_levels_are_9_bit_signed_around_0x80() {
        assert_eq!(dac(0x80, false).channels[5], [0, 0]);
        assert_eq!(dac(0x80, true).channels[5], [1, 1]);
        assert_eq!(dac(0xFF, true).channels[5], [255, 255]);
        assert_eq!(dac(0x00, false).channels[5], [-256, -256]);
        assert_eq!(dac(0x7F, true).channels[5], [-1, -1]);
    }

    #[test]
    fn test_dac_plays_only_while_enabled_and_on_channel_6_only() {
        let mut chip = Ym2612::default();
        chip.write(1, DAC_DATA, 0xFF);
        assert_eq!(chip.sample(), Output::default());
        chip.write(1, DAC_ENABLE, DAC_ON);
        let output = chip.sample();
        assert_eq!(output.channels[5], [254, 254]);
        assert!(output.channels[..5].iter().all(|&levels| levels == [0, 0]));
    }

    #[test]
    fn test_dac_follows_channel_6_panning() {
        let mut chip = Ym2612::default();
        chip.write(1, DAC_ENABLE, DAC_ON);
        chip.write(1, DAC_DATA, 0xC0);
        for (pan, levels) in [(LEFT, [128, 0]), (RIGHT, [0, 128]), (0, [0, 0])] {
            chip.write(2, PAN + 2, pan);
            assert_eq!(chip.sample().channels[5], levels);
        }
        chip.write(1, PAN + 2, 0);
        chip.write(2, PAN + 2, PAN_RESET);
        assert_eq!(chip.sample().channels[5], [128, 128]);
    }

    #[test]
    fn test_reset_turns_the_dac_off_and_pans_every_channel_to_both_sides() {
        let mut chip = Ym2612::default();
        chip.write(1, DAC_ENABLE, DAC_ON);
        chip.write(2, PAN + 2, 0);
        chip.reset();
        assert_eq!(chip, Ym2612::default());
        assert_eq!(chip.register(2, PAN + 2), PAN_RESET);
        assert_eq!(chip.sample(), Output::default());
    }
}
