//! The chip's sound: its register file as the game wrote it, and one stereo output per
//! output period, the FM channels' and the DAC's in channel 6's place. It plays outside the
//! emulation, from the writes the game made, so nothing in a run depends on it.
//!
//! Key on ($28): bits 0-1 the channel in the part bit 2 picks (3 is none), bits 4-7
//! operators 1-4 (YM2608 manual 2-1, Plutiedev); a key takes effect at the next output
//! period, with the registers as they are then [O]. CSM: while channel 3's mode bits are
//! 10, an overflow of timer A sets a key-on flag that the next output period takes and
//! clears, keying all of channel 3's operators on beside their own keys (Nemesis's hardware
//! tests, t=386 p=5650 and p=7967); this half keeps its own copy of the timers, fed the
//! same writes, to know when.
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

use crate::{
    fm::{CHANNELS, Fm, OPERATORS},
    registers::{
        DAC_DATA, DAC_ENABLE, DETUNE_MULTIPLE, KEY_ON, PAN, Registers, TEST, TIMER_CONTROL,
    },
    timer::Timers,
};

/// Register $2B's bit that turns the DAC on.
const DAC_ON: u8 = 0x80;
/// Register $2C's bit that is the DAC's lowest bit.
const DAC_LOW_BIT: u8 = 0x08;
/// A panning register's left and right bits.
const LEFT: u8 = 0x80;
const RIGHT: u8 = 0x40;
/// The panning registers after a reset: both sides on.
const PAN_RESET: u8 = LEFT | RIGHT;
/// Channel 3's mode bits in $27 for CSM.
const CSM_MODE: u8 = 0x80;
/// The channel CSM keys.
const CSM_CHANNEL: usize = 2;

/// One output period: each channel's 9-bit signed level on the left and right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Output {
    pub channels: [[i16; 2]; CHANNELS],
}

impl Output {
    /// The channels added up on each side through the chip's DAC, whose two halves do not
    /// meet at 0 (the "ladder effect"): a level of 0 or above comes out 4 higher, one below 0
    /// 3 lower, so even silence is 4 per channel. The offsets are ymfm's model [O]; testers
    /// only describe the step at 0 as 3-4 times the others (GManiac and Sauraen, t=386
    /// start=795).
    pub fn sides(&self) -> [i32; 2] {
        [0, 1].map(|side| {
            self.channels
                .iter()
                .map(|levels| {
                    let level = i32::from(levels[side]);
                    if level < 0 { level - 3 } else { level + 4 }
                })
                .sum()
        })
    }
}

/// The chip's sound half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ym2612 {
    registers: Registers,
    fm: Fm,
    /// Each operator's key as $28 last set it.
    keys: [[bool; OPERATORS]; CHANNELS],
    /// CSM keys channel 3 on in the next output period.
    csm: bool,
    timers: Timers,
    /// Output periods since power-on, the timers' ticks.
    periods: u64,
}

impl Default for Ym2612 {
    fn default() -> Self {
        Self {
            registers: Registers::new(PAN_RESET),
            fm: Fm::default(),
            keys: [[false; OPERATORS]; CHANNELS],
            csm: false,
            timers: Timers::default(),
            periods: 0,
        }
    }
}

impl Ym2612 {
    /// Every register and the synthesis back to power-on; time goes on, and so does the
    /// envelope clock (ymfm's reset leaves it; no document says what the reset line does to
    /// it) [O].
    pub fn reset(&mut self) {
        let mut fm = std::mem::take(&mut self.fm);
        fm.reset();
        *self = Self {
            fm,
            periods: self.periods,
            ..Self::default()
        };
    }

    /// A data write to `register` of `part` (1 or 2).
    pub fn write(&mut self, part: u8, register: u8, value: u8) {
        self.take_csm();
        self.registers.write(part, register, value);
        // The operators' parameters come from the channel 3 mode and registers $30 up.
        if register >= DETUNE_MULTIPLE || register == TIMER_CONTROL {
            self.fm.invalidate();
        }
        if part != 1 {
            return;
        }
        self.timers.write(register, value, self.periods);
        if register == KEY_ON {
            self.key(value);
        }
    }

    /// Operator `operator` (0-3, operators 1-4) of channel `channel` (0-5) as its attenuation,
    /// its envelope's phase and its phase's top 10 bits, for finding where two renders part.
    #[doc(hidden)]
    pub fn operator_state(&self, channel: usize, operator: usize) -> (u16, String, u32) {
        let (attenuation, envelope, phase) = self.fm.operator(channel, operator).state();
        (attenuation, format!("{envelope:?}"), phase)
    }

    /// The output of the next period.
    pub fn sample(&mut self) -> Output {
        self.take_csm();
        let mut keys = self.keys;
        if std::mem::take(&mut self.csm) {
            keys[CSM_CHANNEL] = [true; OPERATORS];
        }
        let dac = self.register(1, DAC_ENABLE) & DAC_ON != 0;
        let mut output = Output {
            channels: self.fm.sample(&self.registers, &keys, dac),
        };
        if dac {
            let level = self.dac();
            let pan = self.register(2, PAN + 2);
            output.channels[CHANNELS - 1] = [side(pan, LEFT, level), side(pan, RIGHT, level)];
        }
        self.periods += 1;
        output
    }

    /// $28: sets the keys of the channel it names.
    fn key(&mut self, value: u8) {
        let channel = match value & 0x07 {
            low @ 0..=2 => usize::from(low),
            high @ 4..=6 => usize::from(high) - 1,
            _ => return,
        };
        self.keys[channel] = std::array::from_fn(|operator| value >> (4 + operator) & 1 != 0);
    }

    /// Notes a timer A overflow by now while CSM is on.
    fn take_csm(&mut self) {
        let mode = self.register(1, TIMER_CONTROL) & 0xC0;
        if self.timers.take_overflow_a(self.periods) && mode == CSM_MODE {
            self.csm = true;
        }
    }

    /// The DAC's 9-bit signed level: the sample, offset so $80 is 0, above the low bit.
    fn dac(&self) -> i16 {
        let high = u16::from(self.register(1, DAC_DATA) ^ 0x80) << 1;
        let low = u16::from(self.register(1, TEST) & DAC_LOW_BIT != 0);
        ((high | low) << 7) as i16 >> 7
    }

    fn register(&self, part: u8, register: u8) -> u8 {
        self.registers.get(part, register)
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
