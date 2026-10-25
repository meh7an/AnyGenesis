//! The Z80 side as the 68000 sees it, without a Z80: its 8 KiB of RAM, the YM2612 slot and
//! the bus request and reset lines. Sound stays silent, so the YM2612 reads as never busy
//! and drops writes.

/// Bytes of Z80 RAM; the 68000 sees them twice in 0xA00000-0xA03FFF.
pub const RAM_SIZE: usize = 0x2000;

/// What an offset from 0xA00000 (below 0x10000) reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Ram(usize),
    Ym2612,
    Unmapped,
}

impl Target {
    pub(crate) fn of(offset: u32) -> Self {
        match offset {
            0x0000..=0x3FFF => Self::Ram(offset as usize % RAM_SIZE),
            0x4000..=0x5FFF => Self::Ym2612,
            _ => Self::Unmapped,
        }
    }
}

/// Z80 RAM and the two control lines the 68000 drives.
#[derive(Debug)]
pub struct Z80 {
    ram: Box<[u8; RAM_SIZE]>,
    requested: bool,
    reset: bool,
}

impl Default for Z80 {
    fn default() -> Self {
        Self {
            ram: Box::new([0; RAM_SIZE]),
            requested: false,
            reset: true,
        }
    }
}

impl Z80 {
    /// Z80 RAM as the 68000 last wrote it.
    pub fn ram(&self) -> &[u8; RAM_SIZE] {
        &self.ram
    }

    /// Whether the 68000 has asked for the Z80 bus; with no Z80 to wait for, the request is
    /// granted at once.
    pub fn bus_requested(&self) -> bool {
        self.requested
    }

    /// Whether the Z80's reset line is asserted.
    pub fn in_reset(&self) -> bool {
        self.reset
    }

    /// Whether the 68000 may touch Z80 RAM: it holds the bus and the Z80 is out of reset.
    pub fn bus_held(&self) -> bool {
        self.requested && !self.reset
    }

    pub(crate) fn read(&self, index: usize) -> u8 {
        self.ram[index]
    }

    pub(crate) fn write(&mut self, index: usize, value: u8) {
        self.ram[index] = value;
    }

    pub(crate) fn set_requested(&mut self, requested: bool) {
        self.requested = requested;
    }

    pub(crate) fn set_reset(&mut self, reset: bool) {
        self.reset = reset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ram_appears_twice_and_the_ym2612_follows() {
        assert_eq!(Target::of(0x0000), Target::Ram(0));
        assert_eq!(Target::of(0x2001), Target::Ram(1));
        assert_eq!(Target::of(0x3FFF), Target::Ram(0x1FFF));
        assert_eq!(Target::of(0x4000), Target::Ym2612);
        assert_eq!(Target::of(0x5FFF), Target::Ym2612);
        assert_eq!(Target::of(0x6000), Target::Unmapped);
        assert_eq!(Target::of(0x8000), Target::Unmapped);
    }

    #[test]
    fn test_bus_is_held_only_when_requested_and_out_of_reset() {
        let mut z80 = Z80::default();
        assert!(!z80.bus_held());
        z80.set_requested(true);
        assert!(!z80.bus_held());
        z80.set_reset(false);
        assert!(z80.bus_held());
    }
}
