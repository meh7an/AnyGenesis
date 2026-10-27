//! The devices behind one trait: the VDP block at 0xC00000 (with the PSG inside it) and the
//! time and interrupt hooks the scheduler drives. [`Vdp`](crate::vdp::Vdp) implements it.

use std::{any::Any, fmt::Debug};

use anygenesis_abi::{RAM_SIZE, Region, ram_index};
use anygenesis_shared::Size;

use crate::log::AccessKind;

/// Offsets inside the 0x20-byte VDP block, which repeats through 0xC00000-0xDFFFFF.
pub mod port {
    /// Data port (also at 0x02).
    pub const DATA: u8 = 0x00;
    /// Control port (also at 0x06): commands and register writes; reads return the status.
    pub const CONTROL: u8 = 0x04;
    /// HV counter (also at 0x0A-0x0E).
    pub const HV: u8 = 0x08;
    /// The PSG, written by byte at 0x11 (decoded through 0x17).
    pub const PSG: u8 = 0x10;
    /// First port past the ones that take only words: bytes from here on stay bytes.
    pub const BYTE_PORTS: u8 = 0x10;
}

/// The 68000-visible memory a device may read on its own: the source of 68K-to-VDP DMA.
#[derive(Debug, Clone, Copy)]
pub struct Memory<'a> {
    pub rom: &'a [u8],
    pub ram: &'a [u8; RAM_SIZE],
}

impl Memory<'_> {
    /// Reads the word at the 24-bit `address` from ROM or work RAM; `None` elsewhere.
    pub fn read_word(&self, address: u32) -> Option<u16> {
        // The loader refuses images over 4 MiB, so the size fits.
        match Region::of(address, 2, self.rom.len() as u32) {
            Region::Ram => Some(u16::from_be_bytes([
                self.ram[ram_index(address)],
                self.ram[ram_index(address + 1)],
            ])),
            Region::Rom => {
                let at = address as usize;
                Some(u16::from_be_bytes([self.rom[at], self.rom[at + 1]]))
            }
            Region::Bus => None,
        }
    }
}

/// The hardware the memory map hands the VDP block to, and the scheduler's view of it.
///
/// Byte accesses to the data, control and HV ports never arrive as bytes: the map turns a
/// byte write into a word write with the byte in both halves and a byte read into a word
/// read it takes the byte from. From [`port::BYTE_PORTS`] on, sizes pass through.
pub trait Hardware: Any + Debug {
    /// Reads `port` (0x00-0x1F, word reads even).
    fn read(&mut self, port: u8, size: Size) -> u16;
    /// Writes `port`; `memory` is there for DMA from ROM or RAM.
    fn write(&mut self, port: u8, size: Size, value: u16, memory: Memory<'_>);
    /// Moves the hardware's clock on to `clock`, in master clocks since power-on; the
    /// scheduler never moves it back.
    fn advance_to(&mut self, clock: u64);
    /// The master clock after now where an enabled interrupt may next become pending;
    /// `None` while none can. The scheduler runs code up to there before it looks again.
    fn next_interrupt(&self) -> Option<u64>;
    /// The master clock after now where the next frame ends.
    fn next_frame(&self) -> u64;
    /// Whether an access since the last call may have moved [`Hardware::next_interrupt`]
    /// earlier (an enable bit, a reload value, the mode), so the budget must be cut to it.
    fn take_rescheduled(&mut self) -> bool {
        false
    }
    /// Frames that have ended so far: vertical blanking periods begun.
    fn frames(&self) -> u64;
    /// Returns the highest interrupt level the hardware asserts.
    fn pending_interrupt(&self) -> Option<u8>;
    /// The levels the hardware may raise from now on, one bit per level: with none above
    /// the interrupt mask, a wait for an interrupt never ends.
    fn enabled_interrupts(&self) -> u8;
    /// The 68000 took the interrupt at `level`.
    fn acknowledge(&mut self, level: u8);
    /// The reset line: power-on or the RESET instruction.
    fn reset(&mut self);

    /// 68000 cycles the accesses since the last call froze the CPU for, a DMA from 68000
    /// memory for example. The bus charges them to the code that made the access, and the
    /// hardware's clock moves on by them at the next sync, not here.
    fn take_stall(&mut self) -> u32 {
        0
    }

    /// Why the last access was one no rule of the hardware covers, if it was: the bus logs
    /// it with the access's address.
    fn take_fault(&mut self) -> Option<AccessKind> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_reads_rom_and_ram_only() {
        let mut ram = [0; RAM_SIZE];
        ram[0x10] = 0x12;
        ram[0x11] = 0x34;
        let memory = Memory {
            rom: &[0xAB, 0xCD],
            ram: &ram,
        };
        assert_eq!(memory.read_word(0), Some(0xABCD));
        assert_eq!(memory.read_word(0xFF_0010), Some(0x1234));
        assert_eq!(memory.read_word(0xA1_0000), None);
    }
}
