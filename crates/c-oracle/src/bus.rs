//! The memory every side of a run sees: work RAM, ROM, and a device for everything else.
//!
//! Classification follows the generated code's inline fast path (ag_runtime.h): an access
//! goes to RAM when it lies wholly in 0xE00000-0xFFFFFF (byte indexes wrap at 64 KiB, as
//! the mirrors do), to ROM when it lies wholly inside the image, otherwise to the device.
//! Translated code reaches this bus only through the `ag_*` externs, so both sides agree
//! on every access exactly when they make the same accesses.

use std::collections::HashMap;

pub use anygenesis_abi::RAM_START;
use anygenesis_abi::{RAM_SIZE, Region, ram_index};
use anygenesis_shared::{Address, Bus, Size};

use crate::random::SplitMix;

/// Answers the accesses that hit neither RAM nor ROM reads. Sizes are byte or word.
pub trait Device {
    fn read(&mut self, address: Address, size: Size) -> u16;
    fn write(&mut self, address: Address, size: Size, value: u16);
}

/// One device access, in the order made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub write: bool,
    pub size: Size,
    pub address: u32,
    pub value: u16,
    /// The run's cycle count when it happened: the start of its instruction.
    pub time: u64,
}

/// What a bus saw during one instruction, for the lockstep comparison.
#[derive(Debug, Default)]
pub struct Watch {
    /// RAM byte indexes written, possibly repeated.
    pub ram_writes: Vec<usize>,
    /// Whether a word access hit an odd address (an address error on a real 68000).
    pub odd_access: bool,
}

/// RAM, ROM and a device behind the [`Bus`] trait.
pub struct SystemBus<'a> {
    pub ram: &'a mut [u8; RAM_SIZE],
    pub rom: &'a [u8],
    pub device: &'a mut dyn Device,
    pub watch: Option<&'a mut Watch>,
}

impl SystemBus<'_> {
    fn region(&self, address: u32, bytes: u32) -> Region {
        // The image is at most 4 MiB, so its size fits.
        Region::of(address, bytes, self.rom.len() as u32)
    }

    fn note_word(&mut self, address: Address) {
        if let Some(watch) = &mut self.watch {
            watch.odd_access |= !address.is_even();
        }
    }

    fn store_ram(&mut self, address: u32, value: u8) {
        let index = ram_index(address);
        self.ram[index] = value;
        if let Some(watch) = &mut self.watch {
            watch.ram_writes.push(index);
        }
    }
}

impl Bus for SystemBus<'_> {
    fn read_byte(&mut self, address: Address) -> u8 {
        let raw = address.get();
        match self.region(raw, 1) {
            Region::Ram => self.ram[ram_index(raw)],
            Region::Rom => self.rom[raw as usize],
            // A byte read answers in the low byte.
            Region::Bus => self.device.read(address, Size::Byte) as u8,
        }
    }

    fn read_word(&mut self, address: Address) -> u16 {
        self.note_word(address);
        let raw = address.get();
        match self.region(raw, 2) {
            Region::Ram => {
                u16::from_be_bytes([self.ram[ram_index(raw)], self.ram[ram_index(raw + 1)]])
            }
            Region::Rom => u16::from_be_bytes([self.rom[raw as usize], self.rom[raw as usize + 1]]),
            Region::Bus => self.device.read(address, Size::Word),
        }
    }

    fn write_byte(&mut self, address: Address, value: u8) {
        let raw = address.get();
        if self.region(raw, 1) == Region::Ram {
            self.store_ram(raw, value);
        } else {
            self.device.write(address, Size::Byte, value.into());
        }
    }

    fn write_word(&mut self, address: Address, value: u16) {
        self.note_word(address);
        let raw = address.get();
        if self.region(raw, 2) == Region::Ram {
            let [high, low] = value.to_be_bytes();
            self.store_ram(raw, high);
            self.store_ram(raw + 1, low);
        } else {
            self.device.write(address, Size::Word, value);
        }
    }
}

/// I/O for function runs: reads answer from a seeded hash of the address and of how often
/// it was read before, so every side sees the same values whatever else it reads; writes
/// are dropped. Every access is logged with the time its side set in `now`.
#[derive(Debug, Clone)]
pub struct Io {
    seed: u64,
    reads: HashMap<u32, u64>,
    pub log: Vec<Access>,
    /// The cycle count the next access happens at.
    pub now: u64,
}

impl Io {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            reads: HashMap::new(),
            log: Vec::new(),
            now: 0,
        }
    }
}

impl Device for Io {
    fn read(&mut self, address: Address, size: Size) -> u16 {
        let count = self.reads.entry(address.get()).or_default();
        let key = self.seed ^ u64::from(address.get()) << 32 ^ *count;
        *count += 1;
        let value = (SplitMix(key).next_u64() as u32 & size.mask()) as u16;
        self.log.push(Access {
            write: false,
            size,
            address: address.get(),
            value,
            time: self.now,
        });
        value
    }

    fn write(&mut self, address: Address, size: Size, value: u16) {
        self.log.push(Access {
            write: true,
            size,
            address: address.get(),
            value,
            time: self.now,
        });
    }
}

/// Flat 24-bit memory for SingleStepTests cases: bytes never written read as 0.
#[derive(Debug, Default)]
pub struct Sparse(pub HashMap<u32, u8>);

impl Sparse {
    pub fn byte(&self, address: u32) -> u8 {
        self.0.get(&(address & Address::MASK)).copied().unwrap_or(0)
    }

    pub fn set(&mut self, address: u32, value: u8) {
        self.0.insert(address & Address::MASK, value);
    }
}

impl Device for Sparse {
    fn read(&mut self, address: Address, size: Size) -> u16 {
        let raw = address.get();
        match size {
            Size::Byte => self.byte(raw).into(),
            _ => u16::from_be_bytes([self.byte(raw), self.byte(raw + 1)]),
        }
    }

    fn write(&mut self, address: Address, size: Size, value: u16) {
        let raw = address.get();
        let [high, low] = value.to_be_bytes();
        match size {
            Size::Byte => self.set(raw, low),
            _ => {
                self.set(raw, high);
                self.set(raw + 1, low);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus<'a>(ram: &'a mut [u8; RAM_SIZE], rom: &'a [u8], io: &'a mut Io) -> SystemBus<'a> {
        SystemBus {
            ram,
            rom,
            device: io,
            watch: None,
        }
    }

    #[test]
    fn test_long_at_top_of_ram_wraps_into_rom() {
        let mut ram = Box::new([0u8; RAM_SIZE]);
        ram[0xFFFE] = 0x12;
        ram[0xFFFF] = 0x34;
        let rom = [0x56, 0x78];
        let mut io = Io::new(1);
        let value = bus(&mut ram, &rom, &mut io).read_long(Address::new(0xFF_FFFE));
        assert_eq!(value, 0x1234_5678);
        assert!(io.log.is_empty());
    }

    #[test]
    fn test_word_straddling_rom_end_goes_to_the_device() {
        let mut ram = Box::new([0u8; RAM_SIZE]);
        let rom = [0u8; 4];
        let mut io = Io::new(1);
        bus(&mut ram, &rom, &mut io).read_word(Address::new(3));
        assert_eq!(io.log.len(), 1);
    }

    #[test]
    fn test_io_reads_depend_on_address_and_count_only() {
        let mut first = Io::new(7);
        let mut second = Io::new(7);
        let a = Address::new(0xC0_0004);
        let b = Address::new(0xA1_0000);
        let one = first.read(a, Size::Word);
        first.read(b, Size::Word);
        let two = first.read(a, Size::Word);
        // The other side skips the read of `b`: `a` still answers the same.
        assert_eq!(second.read(a, Size::Word), one);
        assert_eq!(second.read(a, Size::Word), two);
    }
}
