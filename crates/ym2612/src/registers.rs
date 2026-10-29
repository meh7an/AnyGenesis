//! The four ports and the registers this crate gives meaning to (Plutiedev).

/// Timer A's high 8 bits.
pub const TIMER_A_HIGH: u8 = 0x24;
/// Timer A's low 2 bits.
pub const TIMER_A_LOW: u8 = 0x25;
/// Timer B.
pub const TIMER_B: u8 = 0x26;
/// Timer control and the channel 3 mode.
pub const TIMER_CONTROL: u8 = 0x27;
/// The DAC's sample.
pub const DAC_DATA: u8 = 0x2A;
/// Bit 7 turns the DAC on in place of channel 6's FM output.
pub const DAC_ENABLE: u8 = 0x2B;
/// The test register; bit 3 is the DAC's ninth, lowest bit.
pub const TEST: u8 = 0x2C;
/// The first of each part's three panning registers ($B4-$B6): bit 7 left, bit 6 right.
pub const PAN: u8 = 0xB4;

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
