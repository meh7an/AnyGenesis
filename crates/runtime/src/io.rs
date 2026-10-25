//! The I/O chip at 0xA10000: the version register and three ports with their serial
//! registers, with a 3-button pad on port 1.
//!
//! Each register is a byte at an odd address (0xA10001, 0xA10003, ...); the even byte beside
//! it answers the same register, so a word read sees the register in both halves.

use std::ops::BitOr;

/// Version register: an overseas (export) console.
pub const VERSION_OVERSEAS: u8 = 0x80;
/// Version register: 50 Hz video.
pub const VERSION_PAL: u8 = 0x40;
/// Version register: no expansion unit (Mega CD) attached.
pub const VERSION_NO_EXPANSION: u8 = 0x20;
/// Version register: the hardware version bits; nonzero consoles have TMSS.
pub const VERSION_HARDWARE: u8 = 0x0F;

/// Bytes of register space the chip decodes; the rest of 0xA10000-0xA10FFF is unmapped.
pub const SPAN: u32 = 0x20;

const REGISTERS: usize = (SPAN / 2) as usize;
const VERSION: usize = 0;
const DATA: usize = 1;
const CONTROL: usize = 4;
/// The TH line: bit 6 of a port's data and control registers.
const TH: u8 = 0x40;
const PINS: u8 = 0x7F;

/// Buttons held on a 3-button pad; a set bit means pressed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons(pub u8);

impl Buttons {
    pub const NONE: Self = Self(0);
    pub const UP: Self = Self(0x01);
    pub const DOWN: Self = Self(0x02);
    pub const LEFT: Self = Self(0x04);
    pub const RIGHT: Self = Self(0x08);
    pub const A: Self = Self(0x10);
    pub const B: Self = Self(0x20);
    pub const C: Self = Self(0x40);
    pub const START: Self = Self(0x80);

    /// Returns whether every button in `other` is held.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Buttons {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The I/O chip's registers and the pad plugged into port 1.
#[derive(Debug)]
pub struct Io {
    version: u8,
    /// Version, data 1-3, control 1-3, then TxData, RxData and serial control per port.
    registers: [u8; REGISTERS],
    pad: Buttons,
}

impl Io {
    /// Returns the chip after reset, reporting `version` in its version register.
    pub fn new(version: u8) -> Self {
        Self {
            version,
            registers: [0; REGISTERS],
            pad: Buttons::NONE,
        }
    }

    /// Clears every port register, as the reset line does.
    pub fn reset(&mut self) {
        self.registers = [0; REGISTERS];
    }

    /// Sets the buttons held on the pad in port 1.
    pub fn set_pad(&mut self, buttons: Buttons) {
        self.pad = buttons;
    }

    /// Reads the register at `offset` (below [`SPAN`]) from 0xA10000.
    pub fn read(&self, offset: u32) -> u8 {
        match register(offset) {
            VERSION => self.version,
            index @ DATA..CONTROL => self.data(index - DATA),
            index => self.registers[index],
        }
    }

    /// Writes the register at `offset` (below [`SPAN`]); the version register ignores it.
    pub fn write(&mut self, offset: u32, value: u8) {
        match register(offset) {
            VERSION => {}
            index => self.registers[index] = value,
        }
    }

    /// A data register reads the latch for pins set as outputs and the lines for inputs.
    /// Bit 7 has no pin and reads back as written.
    fn data(&self, port: usize) -> u8 {
        let latch = self.registers[DATA + port];
        let control = self.registers[CONTROL + port];
        let lines = self.lines(port, latch, control);
        (latch & !PINS) | (latch & control & PINS) | (lines & !control & PINS)
    }

    /// The levels of a port's seven lines; inputs nothing drives are pulled up.
    fn lines(&self, port: usize, latch: u8, control: u8) -> u8 {
        if port != 0 {
            return PINS;
        }
        let th = if control & TH != 0 { latch & TH } else { TH };
        let released = |button: Buttons, bit: u8| if self.pad.contains(button) { 0 } else { bit };
        let directions = released(Buttons::UP, 0x01) | released(Buttons::DOWN, 0x02);
        let buttons = if th != 0 {
            released(Buttons::LEFT, 0x04)
                | released(Buttons::RIGHT, 0x08)
                | released(Buttons::B, 0x10)
                | released(Buttons::C, 0x20)
        } else {
            // With TH low a 3-button pad pulls Left and Right low, which is how software
            // tells a pad is plugged in.
            released(Buttons::A, 0x10) | released(Buttons::START, 0x20)
        };
        th | directions | buttons
    }
}

/// The register index of an offset from 0xA10000: each register owns a word.
fn register(offset: u32) -> usize {
    (offset % SPAN / 2) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA_1: u32 = 0x03;
    const CONTROL_1: u32 = 0x09;
    const DATA_2: u32 = 0x05;

    fn pad(buttons: Buttons) -> Io {
        let mut io = Io::new(0xA1);
        io.set_pad(buttons);
        io.write(CONTROL_1, TH);
        io
    }

    /// Reads port 1 with TH high and low, as games do: Start A 0 0 Down Up, then C B Right
    /// Left Down Up, both active low.
    fn read_both(io: &mut Io) -> (u8, u8) {
        io.write(DATA_1, TH);
        let high = io.read(DATA_1);
        io.write(DATA_1, 0);
        let low = io.read(DATA_1);
        (high, low)
    }

    #[test]
    fn test_version_register_reads_the_configured_value_and_ignores_writes() {
        let mut io = Io::new(0xA1);
        io.write(0x01, 0);
        assert_eq!(io.read(0x01), 0xA1);
        assert_eq!(io.read(0x00), 0xA1);
    }

    #[test]
    fn test_pad_without_buttons_reads_all_released() {
        let mut io = pad(Buttons::NONE);
        assert_eq!(read_both(&mut io), (0x7F, 0x33));
    }

    #[test]
    fn test_pad_reports_each_button_on_its_th_phase() {
        let cases = [
            (Buttons::UP, (0x7E, 0x32)),
            (Buttons::DOWN, (0x7D, 0x31)),
            (Buttons::LEFT, (0x7B, 0x33)),
            (Buttons::RIGHT, (0x77, 0x33)),
            (Buttons::B, (0x6F, 0x33)),
            (Buttons::C, (0x5F, 0x33)),
            (Buttons::A, (0x7F, 0x23)),
            (Buttons::START, (0x7F, 0x13)),
        ];
        for (buttons, expected) in cases {
            let mut io = pad(buttons);
            assert_eq!(read_both(&mut io), expected, "{buttons:?}");
        }
    }

    #[test]
    fn test_pad_with_th_as_input_reads_the_th_high_phase() {
        let mut io = Io::new(0xA1);
        io.set_pad(Buttons::B | Buttons::START);
        assert_eq!(io.read(DATA_1), 0x6F);
    }

    #[test]
    fn test_output_pins_and_bit_7_read_back_the_latch() {
        let mut io = pad(Buttons::NONE);
        io.write(CONTROL_1, 0x7F);
        io.write(DATA_1, 0x85);
        assert_eq!(io.read(DATA_1), 0x85);
    }

    #[test]
    fn test_unconnected_port_reads_pulled_up_lines() {
        let mut io = Io::new(0xA1);
        io.write(DATA_2, 0x80);
        assert_eq!(io.read(DATA_2), 0xFF);
        assert_eq!(io.read(DATA_2 - 1), 0xFF);
    }

    #[test]
    fn test_reset_clears_the_port_registers() {
        let mut io = pad(Buttons::NONE);
        io.write(0x13, 0x30);
        io.reset();
        assert_eq!(io.read(CONTROL_1), 0);
        assert_eq!(io.read(0x13), 0);
    }
}
