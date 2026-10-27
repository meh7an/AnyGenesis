//! The 68000 memory map behind [`Bus`]: what the `ag_*` externs and the interpreter reach.
//!
//! Work RAM and ROM reads are classified exactly as generated code's inline fast path
//! ([`Region`]), so an access lands in the same place whichever side makes it. Everything
//! else is decoded here: cartridge space, the Z80 window, I/O, the Z80 control lines, TMSS
//! and the VDP block, which goes to [`Hardware`]. Unexpected accesses are logged and reads
//! of nothing return the open-bus value.

use anygenesis_abi::{RAM_SIZE, Region, ram_index};
use anygenesis_shared::{Address, Bus, Size};

use crate::{
    hardware::{Hardware, Memory, port},
    io::{self, Io, VERSION_HARDWARE},
    log::{Access, AccessKind, Log},
    timing::CPU_DIVIDER,
    z80::{self, Z80},
};

const Z80_START: u32 = 0xA0_0000;
const IO_START: u32 = 0xA1_0000;
const BUS_REQUEST: u32 = 0xA1_1100;
const Z80_RESET: u32 = 0xA1_1200;
const TMSS_START: u32 = 0xA1_4000;
/// The bit of the bus request and reset registers, in the byte at the even address.
const LINE_BIT: u8 = 0x01;
/// The TMSS key, written as bytes or words to 0xA14000-0xA14003.
const TMSS_KEY: [u8; 4] = *b"SEGA";
/// VDP block addresses with this bit set hang a real console.
const VDP_HANG: u32 = 0x20;
const VDP_PORTS: u32 = 0x1F;

/// Everything outside RAM and ROM the 68000 can reach.
#[derive(Debug)]
pub(crate) struct Devices {
    pub io: Io,
    pub z80: Z80,
    pub tmss: [u8; 4],
    pub hardware: Box<dyn Hardware>,
    pub log: Log,
    pub version: u8,
    pub open_bus: u16,
    /// Accesses that reached a device, so idle detection sees polling.
    pub accesses: u64,
    /// The master clock where the current budget started: the scheduler's time.
    start: u64,
    /// The master clock the hardware has been moved on to.
    synced: u64,
    /// Cycles the hardware froze the 68000 for that the scheduler has not charged yet.
    stall: u32,
}

impl Devices {
    pub fn new(hardware: Box<dyn Hardware>, version: u8, open_bus: u16) -> Self {
        Self {
            io: Io::new(version),
            z80: Z80::default(),
            tmss: [0; 4],
            hardware,
            log: Log::default(),
            version,
            open_bus,
            accesses: 0,
            start: 0,
            synced: 0,
            stall: 0,
        }
    }

    /// Cycles the 68000 spent frozen by the hardware since the last call, for the scheduler
    /// to charge to the budget; the hardware's clock already includes them.
    pub fn take_stall(&mut self) -> i32 {
        i32::try_from(std::mem::take(&mut self.stall)).unwrap_or(i32::MAX)
    }

    /// The master clock `elapsed` 68000 cycles into the current budget.
    pub fn clock_at(&self, elapsed: u32) -> u64 {
        self.start + u64::from(elapsed) * CPU_DIVIDER
    }

    /// Brings the hardware's clock up to `elapsed` cycles into the current budget, so a VDP
    /// access sees the time it happens at rather than the time of the last check point.
    pub fn sync(&mut self, elapsed: u32) {
        let clock = self.clock_at(elapsed);
        if clock > self.synced {
            self.hardware.advance_to(clock);
            self.synced = clock;
        }
    }

    /// Ends the current budget after `used` cycles: the hardware catches up and the next
    /// budget starts there.
    pub fn settle(&mut self, used: u32) {
        self.sync(used);
        self.start = self.clock_at(used);
    }

    /// Whether the VDP is usable: a console without TMSS never locks it.
    pub fn tmss_unlocked(&self) -> bool {
        self.version & VERSION_HARDWARE == 0 || self.tmss == TMSS_KEY
    }

    /// The RESET instruction: the external devices go back to their reset state. TMSS stays
    /// unlocked, since the 68000's RESET does not reach it.
    pub fn reset(&mut self) {
        self.io.reset();
        self.z80 = Z80::default();
        self.hardware.reset();
    }
}

/// RAM, ROM and the devices, for one extern call or interpreted instruction.
pub(crate) struct SystemBus<'a> {
    pub ram: &'a mut [u8; RAM_SIZE],
    pub rom: &'a [u8],
    pub devices: &'a mut Devices,
    /// Cycles used of the current budget when the access is made.
    pub elapsed: u32,
}

impl SystemBus<'_> {
    fn region(&self, address: u32, bytes: u32) -> Region {
        // The loader refuses images over 4 MiB, so the size fits.
        Region::of(address, bytes, self.rom.len() as u32)
    }

    fn note(&mut self, kind: AccessKind, write: bool, size: Size, address: u32) {
        self.devices.log.access(Access {
            kind,
            write,
            size,
            address,
        });
    }

    fn note_odd(&mut self, write: bool, address: Address) {
        if !address.is_even() {
            self.note(AccessKind::OddWord, write, Size::Word, address.get());
        }
    }

    fn open_bus(&self, size: Size) -> u16 {
        match size {
            Size::Byte => self.devices.open_bus & 0xFF,
            _ => self.devices.open_bus,
        }
    }

    /// Reads a byte or word outside RAM and ROM.
    fn device_read(&mut self, address: u32, size: Size) -> u16 {
        self.devices.accesses += 1;
        match address {
            Z80_START..=0xA0_FFFF => self.z80_read(address, size),
            IO_START..=0xA1_0FFF if address - IO_START < io::SPAN => {
                let value = self.devices.io.read(address - IO_START);
                spread(value, size)
            }
            BUS_REQUEST..=0xA1_1101 => self.bus_request_read(address, size),
            Z80_RESET..=0xA1_1201 => self.open_bus(size),
            0xC0_0000..=0xDF_FFFF => self.vdp_read(address, size),
            _ => {
                self.note(AccessKind::Unmapped, false, size, address);
                self.open_bus(size)
            }
        }
    }

    /// Writes a byte or word outside RAM.
    fn device_write(&mut self, address: u32, size: Size, value: u16) {
        self.devices.accesses += 1;
        match address {
            0x00_0000..=0x3F_FFFF => self.note(AccessKind::RomWrite, true, size, address),
            Z80_START..=0xA0_FFFF => self.z80_write(address, size, value),
            IO_START..=0xA1_0FFF if address - IO_START < io::SPAN => {
                // A word write reaches the register through its low byte.
                self.devices.io.write(address - IO_START, value as u8);
            }
            // The memory mode register: DRAM refresh for development hardware only.
            0xA1_1000..=0xA1_1001 => {}
            BUS_REQUEST..=0xA1_1101 => {
                if let Some(level) = line_level(address, size, value) {
                    self.devices.z80.set_requested(level);
                }
            }
            Z80_RESET..=0xA1_1201 => {
                if let Some(level) = line_level(address, size, value) {
                    // Writing 0 asserts reset.
                    self.devices.z80.set_reset(!level);
                }
            }
            TMSS_START..=0xA1_4003 => {
                let at = (address - TMSS_START) as usize;
                match size {
                    Size::Byte => self.devices.tmss[at] = value as u8,
                    _ => {
                        let [high, low] = value.to_be_bytes();
                        self.devices.tmss[at] = high;
                        if let Some(next) = self.devices.tmss.get_mut(at + 1) {
                            *next = low;
                        }
                    }
                }
            }
            // The TMSS cartridge register: switches between the BIOS ROM and the cartridge.
            0xA1_4100..=0xA1_4101 => {}
            0xC0_0000..=0xDF_FFFF => self.vdp_write(address, size, value),
            _ => self.note(AccessKind::Unmapped, true, size, address),
        }
    }

    fn z80_read(&mut self, address: u32, size: Size) -> u16 {
        if size == Size::Word {
            self.note(AccessKind::Z80Word, false, size, address);
        }
        match z80::Target::of(address - Z80_START) {
            z80::Target::Ram(index) => {
                if !self.devices.z80.bus_held() {
                    self.note(AccessKind::Z80WithoutBus, false, size, address);
                }
                spread(self.devices.z80.read(index), size)
            }
            // The YM2612 is silent and never busy.
            z80::Target::Ym2612 => 0,
            z80::Target::Unmapped => {
                self.note(AccessKind::Unmapped, false, size, address);
                self.open_bus(size)
            }
        }
    }

    fn z80_write(&mut self, address: u32, size: Size, value: u16) {
        // A word write stores its high byte: the byte at the even address.
        let byte = match size {
            Size::Byte => value as u8,
            _ => {
                self.note(AccessKind::Z80Word, true, size, address);
                (value >> 8) as u8
            }
        };
        match z80::Target::of(address - Z80_START) {
            z80::Target::Ram(index) => {
                if !self.devices.z80.bus_held() {
                    self.note(AccessKind::Z80WithoutBus, true, size, address);
                }
                self.devices.z80.write(index, byte);
            }
            z80::Target::Ym2612 => {}
            z80::Target::Unmapped => self.note(AccessKind::Unmapped, true, size, address),
        }
    }

    /// The bus request line reads 0 once the bus is granted, in bit 0 of the byte at
    /// 0xA11100 (bit 8 of the word); the other bits are open bus.
    fn bus_request_read(&mut self, address: u32, size: Size) -> u16 {
        let busy = u16::from(!self.devices.z80.bus_requested());
        let open = self.open_bus(size);
        match (size, address & 1) {
            (Size::Byte, 0) => open & !u16::from(LINE_BIT) | busy,
            (Size::Byte, _) => open,
            _ => open & !(u16::from(LINE_BIT) << 8) | busy << 8,
        }
    }

    /// Folds a VDP block address to its port, logging addresses a real console would not
    /// survive; `None` for the ones that hang it.
    fn vdp_port(&mut self, address: u32, write: bool, size: Size) -> Option<u8> {
        if address & VDP_HANG != 0 {
            self.note(AccessKind::VdpHang, write, size, address);
            return None;
        }
        if !self.devices.tmss_unlocked() {
            self.note(AccessKind::VdpLocked, write, size, address);
        }
        self.devices.sync(self.elapsed);
        Some((address & VDP_PORTS) as u8)
    }

    fn vdp_read(&mut self, address: u32, size: Size) -> u16 {
        let Some(port) = self.vdp_port(address, false, size) else {
            return self.open_bus(size);
        };
        let hardware = &mut self.devices.hardware;
        let value = if size == Size::Byte && port < port::BYTE_PORTS {
            let [high, low] = hardware.read(port & !1, Size::Word).to_be_bytes();
            u16::from(if port & 1 == 0 { high } else { low })
        } else {
            hardware.read(port, size)
        };
        self.after_hardware(address, false, size);
        value
    }

    /// Logs what the hardware could not handle and lets time pass for a stall: the
    /// hardware's clock moves on by it at once, and the scheduler charges it later.
    fn after_hardware(&mut self, address: u32, write: bool, size: Size) {
        if let Some(kind) = self.devices.hardware.take_fault() {
            self.note(kind, write, size, address);
        }
        let stall = self.devices.hardware.take_stall();
        if stall > 0 {
            self.elapsed = self.elapsed.saturating_add(stall);
            self.devices.sync(self.elapsed);
            self.devices.stall = self.devices.stall.saturating_add(stall);
        }
    }

    fn vdp_write(&mut self, address: u32, size: Size, value: u16) {
        let Some(port) = self.vdp_port(address, true, size) else {
            return;
        };
        let memory = Memory {
            rom: self.rom,
            ram: self.ram,
        };
        let hardware = &mut self.devices.hardware;
        if size == Size::Byte && port < port::BYTE_PORTS {
            let byte = value & 0xFF;
            hardware.write(port & !1, Size::Word, byte << 8 | byte, memory);
        } else {
            hardware.write(port, size, value, memory);
        }
        self.after_hardware(address, true, size);
    }
}

/// A byte register read as a word answers in both halves.
fn spread(value: u8, size: Size) -> u16 {
    match size {
        Size::Byte => value.into(),
        _ => u16::from_be_bytes([value, value]),
    }
}

/// The level written to a Z80 control line: bit 0 of the byte at the even address, bit 8 of
/// a word; `None` for the byte at the odd address, which holds no line.
fn line_level(address: u32, size: Size, value: u16) -> Option<bool> {
    match (size, address & 1) {
        (Size::Byte, 0) => Some(value as u8 & LINE_BIT != 0),
        (Size::Byte, _) => None,
        _ => Some(value >> 8 & u16::from(LINE_BIT) != 0),
    }
}

impl Bus for SystemBus<'_> {
    fn read_byte(&mut self, address: Address) -> u8 {
        let raw = address.get();
        match self.region(raw, 1) {
            Region::Ram => self.ram[ram_index(raw)],
            Region::Rom => self.rom[raw as usize],
            // A byte read answers in the low byte.
            Region::Bus => self.device_read(raw, Size::Byte) as u8,
        }
    }

    fn read_word(&mut self, address: Address) -> u16 {
        self.note_odd(false, address);
        let raw = address.get();
        match self.region(raw, 2) {
            Region::Ram => {
                u16::from_be_bytes([self.ram[ram_index(raw)], self.ram[ram_index(raw + 1)]])
            }
            Region::Rom => u16::from_be_bytes([self.rom[raw as usize], self.rom[raw as usize + 1]]),
            Region::Bus => self.device_read(raw, Size::Word),
        }
    }

    fn write_byte(&mut self, address: Address, value: u8) {
        let raw = address.get();
        match self.region(raw, 1) {
            Region::Ram => self.ram[ram_index(raw)] = value,
            _ => self.device_write(raw, Size::Byte, value.into()),
        }
    }

    fn write_word(&mut self, address: Address, value: u16) {
        self.note_odd(true, address);
        let raw = address.get();
        match self.region(raw, 2) {
            Region::Ram => {
                let [high, low] = value.to_be_bytes();
                self.ram[ram_index(raw)] = high;
                self.ram[ram_index(raw + 1)] = low;
            }
            _ => self.device_write(raw, Size::Word, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{io::Buttons, vdp::Vdp};

    struct Fixture {
        ram: Box<[u8; RAM_SIZE]>,
        rom: Vec<u8>,
        devices: Devices,
    }

    impl Fixture {
        fn new(version: u8) -> Self {
            let mut rom = vec![0; 0x1000];
            rom[0] = 0x56;
            rom[1] = 0x78;
            Self {
                ram: Box::new([0; RAM_SIZE]),
                rom,
                devices: Devices::new(Box::<Vdp>::default(), version, 0),
            }
        }

        fn bus(&mut self) -> SystemBus<'_> {
            SystemBus {
                ram: &mut self.ram,
                rom: &self.rom,
                devices: &mut self.devices,
                elapsed: 0,
            }
        }

        fn vdp(&self) -> &Vdp {
            (self.devices.hardware.as_ref() as &dyn std::any::Any)
                .downcast_ref()
                .expect("the fixture's hardware is a Vdp")
        }

        fn logged(&self) -> Vec<(AccessKind, bool, u32)> {
            self.devices
                .log
                .accesses()
                .into_iter()
                .map(|(access, _)| (access.kind, access.write, access.address))
                .collect()
        }
    }

    fn at(address: u32) -> Address {
        Address::new(address)
    }

    #[test]
    fn test_ram_mirrors_and_long_wrap_from_ram_into_rom() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_word(at(0xE0_1234), 0xBEEF);
        assert_eq!(bus.read_word(at(0xFF_1234)), 0xBEEF);
        bus.write_word(at(0xFF_FFFE), 0x1234);
        assert_eq!(bus.read_long(at(0xFF_FFFE)), 0x1234_5678);
        assert!(fixture.logged().is_empty());
    }

    #[test]
    fn test_unmapped_reads_return_open_bus_and_are_logged() {
        let mut fixture = Fixture::new(0);
        fixture.devices.open_bus = 0xA5C3;
        let mut bus = fixture.bus();
        assert_eq!(bus.read_word(at(0x40_0000)), 0xA5C3);
        assert_eq!(bus.read_byte(at(0x00_1000)), 0xC3);
        bus.write_byte(at(0x00_0100), 1);
        assert_eq!(
            fixture.logged(),
            [
                (AccessKind::Unmapped, false, 0x00_1000),
                (AccessKind::Unmapped, false, 0x40_0000),
                (AccessKind::RomWrite, true, 0x00_0100),
            ]
        );
    }

    #[test]
    fn test_odd_word_accesses_are_logged_and_still_made() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_word(at(0xFF_0001), 0x1122);
        assert_eq!(bus.read_byte(at(0xFF_0002)), 0x22);
        assert_eq!(fixture.logged(), [(AccessKind::OddWord, true, 0xFF_0001)]);
    }

    #[test]
    fn test_io_registers_answer_bytes_and_words() {
        let mut fixture = Fixture::new(0xA1);
        fixture.devices.io.set_pad(Buttons::START);
        let mut bus = fixture.bus();
        assert_eq!(bus.read_byte(at(0xA1_0001)), 0xA1);
        assert_eq!(bus.read_word(at(0xA1_0000)), 0xA1A1);
        bus.write_byte(at(0xA1_0009), 0x40);
        bus.write_word(at(0xA1_0002), 0x0000);
        assert_eq!(bus.read_byte(at(0xA1_0003)), 0x13);
        assert_eq!(bus.read_byte(at(0xA1_0020)), 0);
        assert_eq!(fixture.logged(), [(AccessKind::Unmapped, false, 0xA1_0020)]);
    }

    #[test]
    fn test_bus_request_reads_granted_while_requested() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        assert_eq!(bus.read_byte(at(0xA1_1100)), 1);
        bus.write_word(at(0xA1_1100), 0x0100);
        assert_eq!(bus.read_byte(at(0xA1_1100)), 0);
        assert_eq!(bus.read_word(at(0xA1_1100)), 0);
        bus.write_byte(at(0xA1_1100), 0);
        assert_eq!(bus.read_word(at(0xA1_1100)), 0x0100);
        assert!(fixture.logged().is_empty());
    }

    #[test]
    fn test_z80_ram_is_stored_and_logged_without_the_bus() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_byte(at(0xA0_0010), 0x11);
        bus.write_word(at(0xA1_1100), 0x0100);
        bus.write_word(at(0xA1_1200), 0x0100);
        bus.write_byte(at(0xA0_2011), 0x22);
        bus.write_word(at(0xA0_0012), 0x3344);
        assert_eq!(bus.read_byte(at(0xA0_0011)), 0x22);
        assert_eq!(bus.read_word(at(0xA0_0012)), 0x3333);
        assert_eq!(bus.read_byte(at(0xA0_4000)), 0);
        assert_eq!(fixture.devices.z80.ram()[0x10..0x13], [0x11, 0x22, 0x33]);
        assert_eq!(
            fixture.logged(),
            [
                (AccessKind::Z80WithoutBus, true, 0xA0_0010),
                (AccessKind::Z80Word, false, 0xA0_0012),
                (AccessKind::Z80Word, true, 0xA0_0012),
            ]
        );
    }

    #[test]
    fn test_tmss_unlocks_with_a_long_write_of_the_key() {
        let mut fixture = Fixture::new(0xA1);
        assert!(!fixture.devices.tmss_unlocked());
        fixture
            .bus()
            .write_long(at(0xA1_4000), u32::from_be_bytes(TMSS_KEY));
        assert!(fixture.devices.tmss_unlocked());
        assert!(Fixture::new(0xA0).devices.tmss_unlocked());
    }

    #[test]
    fn test_vdp_before_tmss_is_logged_on_a_tmss_console() {
        let mut fixture = Fixture::new(0xA1);
        fixture.bus().read_word(at(0xC0_0004));
        assert_eq!(
            fixture.logged(),
            [(AccessKind::VdpLocked, false, 0xC0_0004)]
        );
    }

    #[test]
    fn test_vdp_byte_writes_are_doubled_and_mirrors_fold() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_byte(at(0xC0_0005), 0x8F);
        bus.write_word(at(0xDF_FF46), 0x8C81);
        bus.write_byte(at(0xC0_0011), 0x9F);
        // No cycles pass in this fixture, so the beam stays where power-on left it.
        let status = Vdp::default().status();
        assert_eq!(bus.read_byte(at(0xC0_0004)), (status >> 8) as u8);
        assert_eq!(bus.read_byte(at(0xC0_0005)), status as u8);
        let vdp = fixture.vdp();
        assert_eq!(vdp.registers()[0xF], 0x8F);
        assert_eq!(vdp.registers()[0xC], 0x81);
        assert_eq!(vdp.counts().psg_writes, 1);
        assert_eq!(vdp.counts().status_reads, 2);
        assert!(fixture.logged().is_empty());
    }

    #[test]
    fn test_vdp_faults_are_logged_at_the_access_address() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        // A VRAM write command, then a data read through it.
        bus.write_long(at(0xC0_0004), 0x4000_0000);
        bus.read_word(at(0xC0_0000));
        bus.write_word(at(0xC0_0008), 0);
        assert_eq!(
            fixture.logged(),
            [
                (AccessKind::VdpCommand, false, 0xC0_0000),
                (AccessKind::VdpCommand, true, 0xC0_0008),
            ]
        );
    }

    #[test]
    fn test_dma_stall_moves_the_vdp_clock_and_waits_for_the_scheduler() {
        let mut fixture = Fixture::new(0);
        fixture.ram[0..4].copy_from_slice(&[0x0A, 0xCE, 0x0E, 0xEE]);
        let mut bus = fixture.bus();
        // DMA on, increment 2, length 2 words, source $FF0000, CRAM write at 0.
        for register in [0x8114, 0x8F02, 0x9302, 0x9400, 0x9500, 0x9680, 0x977F] {
            bus.write_word(at(0xC0_0004), register);
        }
        bus.write_long(at(0xC0_0004), 0xC000_0080);
        let elapsed = bus.elapsed;
        let stall = fixture.devices.take_stall();
        assert!(stall > 0);
        assert_eq!(i64::from(elapsed), i64::from(stall));
        let vdp = fixture.vdp();
        assert_eq!(vdp.cram()[..2], [0x0ACE, 0x0EEE]);
        assert_eq!(vdp.timing().clock(), u64::try_from(stall).unwrap() * 7);
        assert_eq!(fixture.devices.take_stall(), 0);
    }

    #[test]
    fn test_vdp_hang_addresses_are_logged_and_not_forwarded() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_word(at(0xC0_0024), 0x8114);
        assert_eq!(bus.read_word(at(0xC0_0020)), 0);
        assert_eq!(fixture.vdp().counts().register_writes, 0);
        assert_eq!(
            fixture.logged(),
            [
                (AccessKind::VdpHang, false, 0xC0_0020),
                (AccessKind::VdpHang, true, 0xC0_0024),
            ]
        );
    }

    #[test]
    fn test_reset_clears_devices_but_keeps_tmss() {
        let mut fixture = Fixture::new(0xA1);
        let mut bus = fixture.bus();
        bus.write_long(at(0xA1_4000), u32::from_be_bytes(TMSS_KEY));
        bus.write_word(at(0xA1_1100), 0x0100);
        bus.write_byte(at(0xA1_0009), 0x40);
        fixture.devices.reset();
        assert!(fixture.devices.tmss_unlocked());
        assert!(!fixture.devices.z80.bus_requested());
        assert_eq!(fixture.devices.io.read(0x09), 0);
    }
}
