//! The devices later milestones grow, behind one trait: the VDP block at 0xC00000 (with the
//! PSG inside it) and the time and interrupt hooks the scheduler drives. Until the VDP exists
//! a stub answers.

use std::{any::Any, fmt::Debug};

use anygenesis_abi::{RAM_SIZE, Region, ram_index};
use anygenesis_shared::Size;

use crate::timing::Timing;

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
    /// Moves the hardware's clock on by `cycles` 68000 cycles.
    fn advance(&mut self, cycles: u32);
    /// 68000 cycles until the next point where an interrupt may become pending, at least 1:
    /// the scheduler runs code that long before it looks again.
    fn until_event(&self) -> u32;
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
}

/// Stands in for the VDP until it is built: it keeps NTSC time and raises the vertical and
/// horizontal interrupts as registers 0, 1 and 10 ask, is otherwise always ready, and counts
/// what it is told.
#[derive(Debug, Clone, Default)]
pub struct StubVdp {
    timing: Timing,
    /// The last value written to each register by a register-write command.
    pub registers: [u8; 24],
    pub register_writes: u64,
    /// Command words (both halves) written to the control port.
    pub command_writes: u64,
    pub data_writes: u64,
    pub status_reads: u64,
    pub psg_writes: u64,
    /// Whether the control port holds the first half of a command.
    pending: bool,
}

impl StubVdp {
    /// The status word apart from the timing bits: FIFO empty, no DMA, NTSC.
    pub const STATUS: u16 = 0x0200;

    /// The beam position and interrupt state.
    pub fn timing(&self) -> &Timing {
        &self.timing
    }

    fn control(&mut self, value: u16) {
        if !self.pending && value & 0xC000 == 0x8000 {
            let register = usize::from(value >> 8 & 0x1F);
            if let Some(slot) = self.registers.get_mut(register) {
                *slot = value as u8;
                self.timing.set_register(register, value as u8);
            }
            self.register_writes += 1;
        } else {
            self.pending = !self.pending;
            self.command_writes += 1;
        }
    }
}

impl Hardware for StubVdp {
    fn read(&mut self, port: u8, _size: Size) -> u16 {
        match port & 0x1C {
            port::CONTROL => {
                // Reading the status ends a half-written command, as on the real VDP.
                self.pending = false;
                self.status_reads += 1;
                Self::STATUS | self.timing.status()
            }
            _ => 0,
        }
    }

    fn write(&mut self, port: u8, _size: Size, value: u16, _memory: Memory<'_>) {
        match port & 0x1C {
            port::DATA => {
                self.pending = false;
                self.data_writes += 1;
            }
            port::CONTROL => self.control(value),
            port::PSG | 0x14 => self.psg_writes += 1,
            _ => {}
        }
    }

    fn advance(&mut self, cycles: u32) {
        self.timing.advance(cycles);
    }

    fn until_event(&self) -> u32 {
        self.timing.until_event()
    }

    fn frames(&self) -> u64 {
        self.timing.frames()
    }

    fn pending_interrupt(&self) -> Option<u8> {
        self.timing.pending()
    }

    fn enabled_interrupts(&self) -> u8 {
        self.timing.enabled()
    }

    fn acknowledge(&mut self, level: u8) {
        self.timing.acknowledge(level);
    }

    fn reset(&mut self) {
        self.pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(vdp: &mut StubVdp, port: u8, value: u16) {
        let ram = [0; RAM_SIZE];
        let memory = Memory {
            rom: &[],
            ram: &ram,
        };
        vdp.write(port, Size::Word, value, memory);
    }

    #[test]
    fn test_stub_vdp_tells_register_writes_from_command_halves() {
        let mut vdp = StubVdp::default();
        write(&mut vdp, port::CONTROL, 0x8114);
        write(&mut vdp, port::CONTROL, 0x4000);
        // The second half of a command may look like a register write.
        write(&mut vdp, port::CONTROL, 0x8000);
        write(&mut vdp, port::CONTROL, 0x8F02);
        assert_eq!(vdp.register_writes, 2);
        assert_eq!(vdp.command_writes, 2);
        assert_eq!(vdp.registers[1], 0x14);
        assert_eq!(vdp.registers[0xF], 0x02);
        assert_eq!(vdp.registers[0], 0);
    }

    #[test]
    fn test_stub_vdp_status_read_ends_a_half_written_command() {
        let mut vdp = StubVdp::default();
        write(&mut vdp, port::CONTROL, 0x4000);
        let status = StubVdp::STATUS | vdp.timing().status();
        assert_eq!(vdp.read(port::CONTROL, Size::Word), status);
        write(&mut vdp, port::CONTROL, 0x8114);
        assert_eq!(vdp.register_writes, 1);
    }

    #[test]
    fn test_stub_vdp_interrupts_follow_its_registers() {
        let mut vdp = StubVdp::default();
        write(&mut vdp, port::CONTROL, 0x8A00);
        write(&mut vdp, port::CONTROL, 0x8014);
        write(&mut vdp, port::CONTROL, 0x8164);
        assert_eq!(vdp.enabled_interrupts(), 1 << 6 | 1 << 4);
        vdp.advance(vdp.until_event());
        assert_eq!(vdp.pending_interrupt(), Some(4));
        vdp.acknowledge(4);
        // A whole frame raises the vertical interrupt, which outranks the horizontal one.
        vdp.advance(130_000);
        assert_eq!(vdp.pending_interrupt(), Some(6));
        assert_eq!(vdp.frames(), 1);
        let status = vdp.read(port::CONTROL, Size::Word);
        assert_ne!(status & crate::timing::status::VINT, 0);
    }

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
