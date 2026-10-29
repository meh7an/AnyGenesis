//! The 68000 memory map behind [`Bus`]: what the `ag_*` externs and the interpreter reach.
//!
//! Work RAM and ROM reads are classified exactly as generated code's inline fast path
//! ([`Region`]), so an access lands in the same place whichever side makes it. Everything
//! else is decoded here: cartridge space, the Z80 side, I/O, the Z80 control lines, TMSS
//! and the VDP block, which goes to [`Hardware`]. Unexpected accesses are logged and reads
//! of nothing return the open-bus value. The Z80 reaches the 68000's side of the bus
//! through [`Window`].

use anygenesis_abi::{RAM_SIZE, Region, ram_index};
use anygenesis_shared::{Address, Bus, Size};

use crate::{
    hardware::{Hardware, Memory, port},
    io::{self, Io, VERSION_HARDWARE},
    log::{Access, AccessKind, Log},
    sound::{Chip, Cpu, SoundLog, SoundWrite},
    timing::CPU_DIVIDER,
    ym2612::Written,
    z80::{self, Main, Z80},
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
/// 68000 wait cycles per access to the Z80's space (Mask of Destiny, SpritesMind).
const Z80_SPACE_WAIT: u32 = 1;
/// What the Z80 reads where nothing on the 68000's side answers it.
const Z80_OPEN: u8 = 0xFF;

/// Everything outside RAM and ROM the 68000 can reach.
#[derive(Debug)]
pub(crate) struct Devices {
    pub io: Io,
    pub z80: Z80,
    pub tmss: [u8; 4],
    pub hardware: Box<dyn Hardware>,
    pub log: Log,
    /// The 68000's writes to the sound chips; the Z80 keeps its own.
    pub sound: SoundLog,
    pub version: u8,
    pub open_bus: u16,
    /// Accesses that reached a device, so idle detection sees polling.
    pub accesses: u64,
    /// The master clock where the current budget started: the scheduler's time.
    start: u64,
    /// The master clock the hardware has been moved on to.
    synced: u64,
    /// Cycles the 68000 lost (to the hardware or to the Z80) that the scheduler has not
    /// charged yet.
    stall: u32,
    /// The master clock from which the 68000's bus is free again after the last stall
    /// that held it; the Z80 waits for it.
    free: u64,
}

impl Devices {
    pub fn new(hardware: Box<dyn Hardware>, version: u8, open_bus: u16) -> Self {
        Self {
            io: Io::new(version),
            z80: Z80::default(),
            tmss: [0; 4],
            hardware,
            log: Log::default(),
            sound: SoundLog::default(),
            version,
            open_bus,
            accesses: 0,
            start: 0,
            synced: 0,
            stall: 0,
            free: 0,
        }
    }

    /// Cycles the 68000 lost since the last call, for the scheduler to charge to the
    /// budget; the hardware's clock already includes those the hardware caused.
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

    /// Whether an access since the last call may have moved an event the budget must end
    /// at earlier: the hardware's next interrupt, or the Z80 starting to run.
    pub fn take_rescheduled(&mut self) -> bool {
        let hardware = self.hardware.take_rescheduled();
        self.z80.take_rescheduled() || hardware
    }

    /// The Z80 and the 68000's side of the bus it reaches, borrowed apart; `now` is the
    /// 68000's time, which the hardware is never moved past.
    fn z80_side<'a>(
        &'a mut self,
        rom: &'a [u8],
        ram: &'a mut [u8; RAM_SIZE],
        now: u64,
    ) -> (&'a mut Z80, Window<'a>) {
        let Self {
            z80,
            hardware,
            log,
            synced,
            free,
            ..
        } = self;
        let window = Window {
            rom,
            ram,
            hardware: &mut **hardware,
            log,
            synced,
            free: *free,
            now,
        };
        (z80, window)
    }

    /// Runs the Z80 up to the budget's start, the 68000's time after [`Devices::settle`];
    /// the cycles its accesses to the 68000's bus took wait for the scheduler.
    pub fn sync_z80(&mut self, rom: &[u8], ram: &mut [u8; RAM_SIZE]) {
        let now = self.start;
        let (z80, mut window) = self.z80_side(rom, ram, now);
        let steals = z80.sync(now, &mut window);
        self.stall = self.stall.saturating_add(steals);
    }

    /// Where the Z80 next reaches the 68000's bus, looking as far as `until`.
    pub fn z80_event(&mut self, rom: &[u8], ram: &mut [u8; RAM_SIZE], until: u64) -> Option<u64> {
        let now = self.start;
        let (z80, window) = self.z80_side(rom, ram, now);
        z80.next_event(until, &window)
    }

    /// Whether the VDP is usable: a console without TMSS never locks it.
    pub fn tmss_unlocked(&self) -> bool {
        self.version & VERSION_HARDWARE == 0 || self.tmss == TMSS_KEY
    }

    /// The RESET instruction: the external devices go back to their reset state. TMSS stays
    /// unlocked, since the 68000's RESET does not reach it; the sound logs stay as well.
    pub fn reset(&mut self) {
        self.io.reset();
        let sound = std::mem::take(self.z80.sound_mut());
        self.z80 = Z80::default();
        *self.z80.sound_mut() = sound;
        self.hardware.reset();
    }
}

/// The 68000's side of the bus as the Z80 reaches it: through the bank window and the VDP
/// area. Work RAM can be written this way but reads back $FF; the Z80's own space hangs a
/// console, which is logged instead.
pub(crate) struct Window<'a> {
    rom: &'a [u8],
    ram: &'a mut [u8; RAM_SIZE],
    hardware: &'a mut dyn Hardware,
    log: &'a mut Log,
    synced: &'a mut u64,
    free: u64,
    /// The 68000's time: the hardware is moved on to an access's time, but not past it.
    now: u64,
}

impl Window<'_> {
    fn note(&mut self, kind: AccessKind, write: bool, address: u32) {
        self.log.access(Access {
            kind,
            write,
            size: Size::Byte,
            address,
        });
    }

    /// Folds a VDP block address to its port after bringing the hardware up to `clock`;
    /// `None` for the addresses that hang a console.
    fn vdp_port(&mut self, address: u32, write: bool, clock: u64) -> Option<u8> {
        if address & VDP_HANG != 0 {
            self.note(AccessKind::VdpHang, write, address);
            return None;
        }
        let clock = clock.min(self.now);
        if clock > *self.synced {
            self.hardware.advance_to(clock);
            *self.synced = clock;
        }
        Some((address & VDP_PORTS) as u8)
    }

    /// Logs what the hardware could not handle. A stall the access caused would hold the
    /// Z80, not the 68000, and is not modelled.
    fn after_hardware(&mut self, address: u32, write: bool) {
        if let Some(kind) = self.hardware.take_fault() {
            self.note(kind, write, address);
        }
        self.hardware.take_stall();
    }
}

impl Main for Window<'_> {
    fn read(&mut self, address: u32, clock: u64) -> u8 {
        match address {
            0x00_0000..=0x3F_FFFF => self.peek(address),
            Z80_START..=0xA0_FFFF => {
                self.note(AccessKind::Z80Hang, false, address);
                Z80_OPEN
            }
            0xC0_0000..=0xDF_FFFF => {
                let Some(port) = self.vdp_port(address, false, clock) else {
                    return Z80_OPEN;
                };
                let value = vdp_read_byte(self.hardware, port);
                self.after_hardware(address, false);
                value
            }
            0xE0_0000..=0xFF_FFFF => Z80_OPEN,
            _ => {
                self.note(AccessKind::Unmapped, false, address);
                Z80_OPEN
            }
        }
    }

    fn write(&mut self, address: u32, value: u8, clock: u64) {
        match address {
            0x00_0000..=0x3F_FFFF => self.note(AccessKind::RomWrite, true, address),
            Z80_START..=0xA0_FFFF => self.note(AccessKind::Z80Hang, true, address),
            0xC0_0000..=0xDF_FFFF => {
                let Some(port) = self.vdp_port(address, true, clock) else {
                    return;
                };
                let memory = Memory {
                    rom: self.rom,
                    ram: self.ram,
                };
                vdp_write_byte(self.hardware, port, value, memory);
                self.after_hardware(address, true);
            }
            0xE0_0000..=0xFF_FFFF => self.ram[ram_index(address)] = value,
            _ => self.note(AccessKind::Unmapped, true, address),
        }
    }

    fn peek(&self, address: u32) -> u8 {
        self.rom.get(address as usize).copied().unwrap_or(Z80_OPEN)
    }

    fn free_from(&self) -> u64 {
        self.free
    }

    fn interrupt(&self, clock: u64) -> bool {
        self.hardware.z80_interrupt(clock)
    }

    fn is_psg(&self, address: u32) -> bool {
        is_psg(address)
    }
}

/// Whether `address` is one of the VDP block's PSG ports.
fn is_psg(address: u32) -> bool {
    (0xC0_0000..=0xDF_FFFF).contains(&address)
        && address & VDP_HANG == 0
        && (address & VDP_PORTS) as u8 & !7 == port::PSG
}

/// A byte read of a VDP port: the data, control and HV ports answer words, of which the
/// byte at the even address is the high half.
fn vdp_read_byte(hardware: &mut dyn Hardware, port: u8) -> u8 {
    if port < port::BYTE_PORTS {
        let [high, low] = hardware.read(port & !1, Size::Word).to_be_bytes();
        if port & 1 == 0 { high } else { low }
    } else {
        hardware.read(port, Size::Byte) as u8
    }
}

/// A byte write to a VDP port: the data, control and HV ports take it as a word with the
/// byte in both halves.
fn vdp_write_byte(hardware: &mut dyn Hardware, port: u8, value: u8, memory: Memory<'_>) {
    if port < port::BYTE_PORTS {
        let byte = u16::from(value);
        hardware.write(port & !1, Size::Word, byte << 8 | byte, memory);
    } else {
        hardware.write(port, Size::Byte, value.into(), memory);
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

    /// The master clock of the access.
    fn now(&self) -> u64 {
        self.devices.clock_at(self.elapsed)
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

    /// The 68000 waits `cycles` more for the access: the scheduler charges them later.
    fn wait(&mut self, cycles: u32) {
        self.elapsed = self.elapsed.saturating_add(cycles);
        self.devices.stall = self.devices.stall.saturating_add(cycles);
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
                    self.bus_request(level);
                }
            }
            Z80_RESET..=0xA1_1201 => {
                if let Some(level) = line_level(address, size, value) {
                    // Writing 0 asserts reset.
                    self.z80_reset(!level);
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

    /// The Z80's side as the 68000 reaches it at `address`, or `None` when the access must
    /// not reach it: nothing answers there, it would hang a console, or the 68000 does not
    /// hold the Z80's bus (each logged).
    fn z80_target(&mut self, address: u32, write: bool, size: Size) -> Option<z80::Target> {
        let held = self.devices.z80.bus_held(self.now());
        self.wait(Z80_SPACE_WAIT);
        if size == Size::Word {
            self.note(AccessKind::Z80Word, write, size, address);
        }
        let target = z80::Target::of(address - Z80_START);
        let kind = match target {
            z80::Target::Unmapped => AccessKind::Unmapped,
            z80::Target::Hang => AccessKind::Z80Hang,
            _ if !held => AccessKind::Z80WithoutBus,
            _ => return Some(target),
        };
        self.note(kind, write, size, address);
        None
    }

    fn z80_read(&mut self, address: u32, size: Size) -> u16 {
        let now = self.now();
        let value = match self.z80_target(address, false, size) {
            // Reads without the bus see the open bus.
            None => return self.open_bus(size),
            Some(z80::Target::Ram(index)) => self.devices.z80.read(index),
            Some(z80::Target::Ym2612(offset)) => self.devices.z80.ym_status(offset, now),
            Some(_) => Z80_OPEN,
        };
        spread(value, size)
    }

    fn z80_write(&mut self, address: u32, size: Size, value: u16) {
        // A word write stores its high byte: the byte at the even address.
        let byte = match size {
            Size::Byte => value as u8,
            _ => (value >> 8) as u8,
        };
        let now = self.now();
        match self.z80_target(address, true, size) {
            Some(z80::Target::Ram(index)) => self.devices.z80.write(index, byte),
            Some(z80::Target::Ym2612(offset)) => {
                let written = self.devices.z80.ym_write(offset, byte, now);
                self.record_ym(written, byte, now, size, address);
            }
            Some(z80::Target::Bank) => self.devices.z80.write_bank(byte),
            // Writes without the bus are ignored.
            _ => {}
        }
    }

    /// Logs a YM2612 write the 68000 made at `clock`, an access of `size` to `address`.
    fn record_ym(&mut self, written: Written, value: u8, clock: u64, size: Size, address: u32) {
        if written.timer {
            self.note(AccessKind::YmTimer, true, size, address);
            self.devices.sound.timer(Cpu::M68000);
        }
        if let Some((part, register)) = written.data {
            self.devices.sound.record(SoundWrite {
                clock,
                cpu: Cpu::M68000,
                chip: Chip::Ym2612 { part, register },
                value,
            });
        }
    }

    /// The bus request line moves to `level`. The Z80's accesses to the 68000's bus up to
    /// the grant take their cycles from the 68000 as well.
    fn bus_request(&mut self, level: bool) {
        let now = self.now();
        if !level {
            self.devices.z80.release(now);
            return;
        }
        let (z80, mut window) = self.devices.z80_side(self.rom, self.ram, now);
        let steals = z80.request(now, &mut window);
        self.devices.stall = self.devices.stall.saturating_add(steals);
    }

    /// The Z80's reset line moves to `asserted`.
    fn z80_reset(&mut self, asserted: bool) {
        let now = self.now();
        if !asserted {
            self.devices.z80.release_reset(now);
            return;
        }
        let (z80, mut window) = self.devices.z80_side(self.rom, self.ram, now);
        let steals = z80.assert_reset(now, &mut window);
        self.devices.stall = self.devices.stall.saturating_add(steals);
    }

    /// The bus request line reads 0 once the bus is granted, in bit 0 of the byte at
    /// 0xA11100 (bit 8 of the word); the other bits are open bus. The Z80 in reset never
    /// grants it.
    fn bus_request_read(&mut self, address: u32, size: Size) -> u16 {
        let busy = u16::from(!self.devices.z80.bus_held(self.now()));
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
        let hardware = &mut *self.devices.hardware;
        let value = if size == Size::Byte {
            vdp_read_byte(hardware, port).into()
        } else {
            hardware.read(port, size)
        };
        self.after_hardware(address, false, size);
        value
    }

    /// Logs what the hardware could not handle and lets time pass for a stall: the
    /// hardware's clock moves on by it at once, and the scheduler charges it later. The
    /// 68000's bus is held until it ends, which a Z80 access through its window waits out.
    fn after_hardware(&mut self, address: u32, write: bool, size: Size) {
        if let Some(kind) = self.devices.hardware.take_fault() {
            self.note(kind, write, size, address);
        }
        let stall = self.devices.hardware.take_stall();
        if stall > 0 {
            self.elapsed = self.elapsed.saturating_add(stall);
            self.devices.sync(self.elapsed);
            self.devices.stall = self.devices.stall.saturating_add(stall);
            self.devices.free = self.devices.synced;
        }
    }

    fn vdp_write(&mut self, address: u32, size: Size, value: u16) {
        let Some(port) = self.vdp_port(address, true, size) else {
            return;
        };
        if is_psg(address) {
            let clock = self.now();
            self.devices.sound.record(SoundWrite {
                clock,
                cpu: Cpu::M68000,
                chip: Chip::Psg,
                value: value as u8,
            });
        }
        let memory = Memory {
            rom: self.rom,
            ram: self.ram,
        };
        let hardware = &mut *self.devices.hardware;
        if size == Size::Byte {
            vdp_write_byte(hardware, port, value as u8, memory);
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

    /// Releases the Z80's reset and takes its bus, as a game does before loading it.
    fn take_z80_bus(bus: &mut SystemBus<'_>) {
        bus.write_word(at(0xA1_1200), 0x0100);
        bus.write_word(at(0xA1_1100), 0x0100);
    }

    #[test]
    fn test_bus_request_is_granted_only_out_of_reset() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        assert_eq!(bus.read_byte(at(0xA1_1100)), 1);
        bus.write_word(at(0xA1_1100), 0x0100);
        assert_eq!(
            bus.read_byte(at(0xA1_1100)),
            1,
            "reset outranks the request"
        );
        bus.write_byte(at(0xA1_1200), 1);
        assert_eq!(bus.read_byte(at(0xA1_1100)), 0);
        assert_eq!(bus.read_word(at(0xA1_1100)), 0);
        bus.write_byte(at(0xA1_1100), 0);
        assert_eq!(bus.read_word(at(0xA1_1100)), 0x0100);
        assert!(fixture.logged().is_empty());
    }

    #[test]
    fn test_z80_space_costs_the_68000_a_wait_cycle_per_access() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.read_byte(at(0xA0_0000));
        bus.write_long(at(0xA0_0010), 0);
        bus.read_byte(at(0xA1_1100));
        assert_eq!(bus.elapsed, 3);
        assert_eq!(fixture.devices.take_stall(), 3);
    }

    #[test]
    fn test_z80_ram_needs_the_bus_and_takes_bytes() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_byte(at(0xA0_0010), 0x11);
        assert_eq!(bus.read_byte(at(0xA0_0010)), 0, "open bus");
        take_z80_bus(&mut bus);
        bus.write_byte(at(0xA0_2011), 0x22);
        bus.write_word(at(0xA0_0012), 0x3344);
        bus.write_byte(at(0xA0_8013), 0x55);
        assert_eq!(bus.read_byte(at(0xA0_0011)), 0x22);
        assert_eq!(bus.read_word(at(0xA0_0012)), 0x3333);
        bus.read_byte(at(0xA0_7F11));
        assert_eq!(fixture.devices.z80.ram()[0x10..0x14], [0, 0x22, 0x33, 0x55]);
        assert_eq!(
            fixture.logged(),
            [
                (AccessKind::Z80WithoutBus, false, 0xA0_0010),
                (AccessKind::Z80WithoutBus, true, 0xA0_0010),
                (AccessKind::Z80Word, false, 0xA0_0012),
                (AccessKind::Z80Word, true, 0xA0_0012),
                (AccessKind::Z80Hang, false, 0xA0_7F11),
            ]
        );
    }

    #[test]
    fn test_bank_register_shifts_in_bit_0_of_each_write() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        take_z80_bus(&mut bus);
        for bit in [1, 0, 0, 0, 0, 0, 0, 0, 1] {
            bus.write_byte(at(0xA0_6000), bit);
        }
        assert_eq!(bus.read_byte(at(0xA0_6000)), 0xFF);
        assert_eq!(fixture.devices.z80.bank(), 0x101);
    }

    #[test]
    fn test_ym2612_reads_busy_for_192_cycles_after_a_data_write() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        take_z80_bus(&mut bus);
        bus.elapsed = 10;
        bus.write_byte(at(0xA0_4000), 0x2B);
        assert_eq!(bus.read_byte(at(0xA0_4000)), 0);
        // The data write is made at 12, after the two waits before it.
        bus.write_byte(at(0xA0_4001), 0x80);
        for (address, status) in [(0xA0_4000, 0x80), (0xA0_5FFC, 0x80), (0xA0_4002, 0)] {
            bus.elapsed = 12 + 191;
            assert_eq!(bus.read_byte(at(address)), status, "{address:06X}");
        }
        bus.elapsed = 12 + 192;
        assert_eq!(bus.read_byte(at(0xA0_4000)), 0);
        assert!(fixture.logged().is_empty());
        let counts = fixture.devices.sound.counts();
        assert_eq!(counts.ym2612, [1, 0]);
    }

    #[test]
    fn test_ym2612_timer_runs_are_logged() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        take_z80_bus(&mut bus);
        bus.write_byte(at(0xA0_4000), 0x27);
        bus.write_byte(at(0xA0_4001), 0x00);
        bus.write_byte(at(0xA0_4001), 0x15);
        assert_eq!(fixture.logged(), [(AccessKind::YmTimer, true, 0xA0_4001)]);
        assert_eq!(fixture.devices.sound.counts().timers, [1, 0]);
    }

    #[test]
    fn test_the_z80_reads_rom_through_its_window_at_the_68000s_cost() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        take_z80_bus(&mut bus);
        // LD A,(0x8001); LD (0x1000),A; HALT, with the bank at 0.
        for (offset, byte) in [0x3A, 0x01, 0x80, 0x32, 0x00, 0x10, 0x76]
            .into_iter()
            .enumerate()
        {
            bus.write_byte(at(0xA0_0000 + offset as u32), byte);
        }
        bus.write_byte(at(0xA1_1100), 0);
        fixture.devices.take_stall();
        fixture.devices.settle(100);
        fixture.devices.sync_z80(&fixture.rom, &mut fixture.ram);
        assert_eq!(fixture.devices.z80.ram()[0x1000], 0x78);
        assert_eq!(fixture.devices.take_stall(), 11);
    }

    #[test]
    fn test_psg_writes_of_the_68000_are_counted() {
        let mut fixture = Fixture::new(0);
        let mut bus = fixture.bus();
        bus.write_byte(at(0xC0_0011), 0x9F);
        bus.write_byte(at(0xC0_0013), 0xBF);
        assert_eq!(fixture.devices.sound.counts().psg, [2, 0]);
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
