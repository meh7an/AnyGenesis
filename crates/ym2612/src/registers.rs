//! The four ports, the registers this crate gives meaning to, and the register file of both
//! parts (Plutiedev; YM2608 manual 2-2 to 2-5).
//!
//! A frequency's high byte ($A4-$A6, and $AC-$AE for channel 3's operators) does not take
//! effect when written: it waits in a latch until its low byte is written (Plutiedev).
//! Settled by ymfm [O]: one latch for each of the two groups, shared by both parts.

/// Timer A's high 8 bits.
pub const TIMER_A_HIGH: u8 = 0x24;
/// Timer A's low 2 bits.
pub const TIMER_A_LOW: u8 = 0x25;
/// Timer B.
pub const TIMER_B: u8 = 0x26;
/// Timer control and the channel 3 mode.
pub const TIMER_CONTROL: u8 = 0x27;
/// The LFO: bit 3 on, bits 0-2 the rate.
pub const LFO: u8 = 0x22;
/// Key on and off: bits 0-2 the channel, bits 4-7 the operators.
pub const KEY_ON: u8 = 0x28;
/// The DAC's sample.
pub const DAC_DATA: u8 = 0x2A;
/// Bit 7 turns the DAC on in place of channel 6's FM output.
pub const DAC_ENABLE: u8 = 0x2B;
/// The test register; bit 3 is the DAC's ninth, lowest bit.
pub const TEST: u8 = 0x2C;
/// The first of each part's three panning registers ($B4-$B6): bit 7 left, bit 6 right,
/// bits 4-5 the amplitude modulation's depth, bits 0-2 the phase modulation's.
pub const PAN: u8 = 0xB4;

/// Per-operator registers, by their first address.
pub(crate) const DETUNE_MULTIPLE: u8 = 0x30;
pub(crate) const TOTAL_LEVEL: u8 = 0x40;
pub(crate) const KEY_SCALE_ATTACK: u8 = 0x50;
pub(crate) const DECAY: u8 = 0x60;
pub(crate) const SUSTAIN_RATE: u8 = 0x70;
pub(crate) const SUSTAIN_RELEASE: u8 = 0x80;
pub(crate) const SSG_EG: u8 = 0x90;
/// Per-channel registers, by their first address.
pub(crate) const FREQUENCY_LOW: u8 = 0xA0;
pub(crate) const FREQUENCY_HIGH: u8 = 0xA4;
pub(crate) const SPECIAL_LOW: u8 = 0xA8;
pub(crate) const SPECIAL_HIGH: u8 = 0xAC;
pub(crate) const ALGORITHM: u8 = 0xB0;

/// Each operator's register offset within its channel's: operators 1, 3, 2 and 4 come in
/// that order (Plutiedev).
const OPERATOR_OFFSETS: [u8; 4] = [0, 8, 4, 12];

/// Both parts' registers as the chip uses them, the frequency latches applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registers {
    values: [[u8; 256]; 2],
    /// The latched high bytes: the channels' and channel 3's operators'.
    latches: [u8; 2],
}

impl Registers {
    /// Every register 0, as at power-on, but the panning registers with both sides on.
    pub fn new(pan: u8) -> Self {
        let mut registers = Self {
            values: [[0; 256]; 2],
            latches: [0; 2],
        };
        for part in &mut registers.values {
            part[usize::from(PAN)..usize::from(PAN) + 3].fill(pan);
        }
        registers
    }

    /// A data write to `register` of `part` (1 or 2).
    pub fn write(&mut self, part: u8, register: u8, value: u8) {
        let part = usize::from(part - 1);
        match register {
            FREQUENCY_HIGH..=0xA6 => self.latches[0] = value,
            SPECIAL_HIGH..=0xAE => self.latches[1] = value,
            FREQUENCY_LOW..=0xA2 | SPECIAL_LOW..=0xAA => {
                let latch = self.latches[usize::from(register >= SPECIAL_LOW)];
                self.values[part][usize::from(register)] = value;
                self.values[part][usize::from(register + 4)] = latch;
            }
            _ => self.values[part][usize::from(register)] = value,
        }
    }

    /// Register `register` of `part` (1 or 2).
    pub fn get(&self, part: u8, register: u8) -> u8 {
        self.values[usize::from(part - 1)][usize::from(register)]
    }

    /// Channel `channel`'s (0-5) register in the group starting at `base`.
    pub fn channel(&self, channel: usize, base: u8) -> u8 {
        self.get(part_of(channel), base + (channel % 3) as u8)
    }

    /// Operator `operator`'s (0-3, operators 1-4) register of channel `channel` in the group
    /// starting at `base`.
    pub fn operator(&self, channel: usize, operator: usize, base: u8) -> u8 {
        self.channel(channel, base + OPERATOR_OFFSETS[operator])
    }

    /// Channel `channel`'s block and frequency number: 3 bits above 11.
    pub fn frequency(&self, channel: usize) -> u16 {
        self.pair(part_of(channel), FREQUENCY_LOW + (channel % 3) as u8)
    }

    /// The block and frequency number in channel 3's special-mode pair `index` (0-2,
    /// $A8/$AC to $AA/$AE).
    pub fn special_frequency(&self, index: usize) -> u16 {
        self.pair(1, SPECIAL_LOW + index as u8)
    }

    fn pair(&self, part: u8, low: u8) -> u16 {
        u16::from(self.get(part, low + 4) & 0x3F) << 8 | u16::from(self.get(part, low))
    }
}

/// The part (1 or 2) channel `channel` (0-5) is in.
fn part_of(channel: usize) -> u8 {
    1 + (channel / 3) as u8
}

/// One of the four ports, from the low two bits of an address in the chip's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    AddressOne,
    DataOne,
    AddressTwo,
    DataTwo,
}

impl Port {
    pub fn of(offset: u32) -> Self {
        match offset & 3 {
            0 => Self::AddressOne,
            1 => Self::DataOne,
            2 => Self::AddressTwo,
            _ => Self::DataTwo,
        }
    }
}
