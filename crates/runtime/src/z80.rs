//! The Z80 side of the console: the Z80 (crates/z80), its 8 KiB of RAM, the YM2612 on its
//! bus, the bank register and the window into 68000 space, and the bus request and reset
//! lines the 68000 drives.
//!
//! Time. The Z80 runs on master clock / 15, ahead of the 68000: up to the next instruction
//! that reaches the 68000's bus (the bank window at 0x8000, the VDP and PSG at 0x7F00), or
//! up to where the 68000's budget ends when none does. Such an instruction is a scheduler
//! event ([`Z80::next_event`]): it runs once the 68000 has got to its first access there,
//! with the 68000's bus attached, and the 68000 then loses [`MAIN_STEAL`] cycles per access
//! at that instruction boundary. Everything else the Z80 touches (its RAM, the YM2612, the
//! bank register) answers the 68000 only while the 68000 holds the Z80's bus, so only the
//! bus request and reset lines change what a running Z80 sees. When one of them moves at a
//! time the Z80 has already run past, the Z80 goes back to the instruction running then
//! (a journal of its states and an undo log of its RAM writes) and goes on from there.
//!
//! Instructions stay whole: a bus request is granted at the end of the machine cycle in
//! progress where it lands (the Z80 samples BUSREQ on the last T-state of each machine
//! cycle), but that instruction's later cycles still run before the Z80 stops.
//!
//! Sources (behaviour only; side-session notes v0.2-m3.2-research-*.md list every URL):
//! Charles MacDonald's Genesis hardware notes (the map, the bank register, bus request and
//! reset reads, open-bus and ignored accesses without the bus, the word access rule, the
//! interrupt from the VDP and the $FF acknowledge byte), the Zilog Z80 user manual (BUSREQ
//! sampling, reset), Mask of Destiny's SpritesMind captures (one 68000 wait per access to
//! the Z80's space, about 3 Z80 wait states per bank access, no priority over DMA, the
//! bank register's shift direction), Kabuto's hardware notes (11 68000 cycles lost per Z80
//! access on average) and MacDonald's SpritesMind post on the interrupt lasting one line.
//! Chosen, unverified: a flat 3 T wait and 11-cycle steal per access, and the grant coming
//! at once when reset releases while the bus is requested.

use anygenesis_ym2612::{Control, Written};
use anygenesis_z80::{Bus as CoreBus, Z80 as Core};

use crate::sound::{Chip, Cpu, SoundCounts, SoundLog, SoundWrite};

/// Bytes of Z80 RAM; both CPUs see them twice in the first 16 KiB of the Z80's space.
pub const RAM_SIZE: usize = 0x2000;
/// Master clocks per Z80 T-state.
pub const CLOCK_DIVIDER: u64 = 15;
/// Wait states one Z80 access to the 68000's bus takes while that bus is free.
pub const MAIN_WAIT: u32 = 3;
/// 68000 cycles one Z80 access to the 68000's bus takes from the 68000.
pub const MAIN_STEAL: u32 = 11;
/// What reads of nothing return on the Z80's bus.
const OPEN: u8 = 0xFF;
/// Bits of the bank register: 68000 address bits 15-23.
const BANK_BITS: u32 = 9;
/// The 68000 address of the VDP block the Z80 reaches at 0x7F00.
const VDP_BASE: u32 = 0xC0_0000;
/// The low 15 address bits, which pass through the bank window unchanged.
const WINDOW_MASK: u32 = 0x7FFF;

/// What an offset from 0xA00000 (below 0x10000) reaches from the 68000. Address bit 15 is
/// held low on the way, so the upper half repeats the lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Ram(usize),
    /// The offset into the YM2612 slot, whose four ports repeat through it.
    Ym2612(u32),
    /// The bank register, written one bit at a time.
    Bank,
    /// The Z80's view of the VDP, which hangs a console when the 68000 reaches it this way.
    Hang,
    Unmapped,
}

impl Target {
    pub(crate) fn of(offset: u32) -> Self {
        match offset & WINDOW_MASK {
            offset @ 0x0000..=0x3FFF => Self::Ram(offset as usize % RAM_SIZE),
            offset @ 0x4000..=0x5FFF => Self::Ym2612(offset - 0x4000),
            0x6000..=0x60FF => Self::Bank,
            0x7F00..=0x7FFF => Self::Hang,
            _ => Self::Unmapped,
        }
    }
}

/// What a Z80 address reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    Ram(usize),
    Ym2612(u32),
    Bank,
    Nothing,
    /// The 68000's bus at this 24-bit address.
    Main(u32),
}

impl Reach {
    fn of(address: u16, bank: u16) -> Self {
        match address {
            0x0000..=0x3FFF => Self::Ram(usize::from(address) % RAM_SIZE),
            0x4000..=0x5FFF => Self::Ym2612(u32::from(address - 0x4000)),
            0x6000..=0x60FF => Self::Bank,
            0x6100..=0x7EFF => Self::Nothing,
            0x7F00..=0x7FFF => Self::Main(VDP_BASE | u32::from(address & 0xFF)),
            _ => Self::Main(u32::from(bank) << 15 | u32::from(address) & WINDOW_MASK),
        }
    }
}

/// The bank register after a write of `value`: bit 0 shifts in at the top, so after nine
/// writes the first one sits in address bit 15.
fn shift_bank(bank: u16, value: u8) -> u16 {
    bank >> 1 | u16::from(value & 1) << (BANK_BITS - 1)
}

/// The 68000's bus as the Z80 reaches it, and the VDP's interrupt line into the Z80.
pub(crate) trait Main {
    /// Reads the byte at 68000 address `address`, for an access at master clock `clock`.
    fn read(&mut self, address: u32, clock: u64) -> u8;
    /// Writes the byte at 68000 address `address`, for an access at master clock `clock`.
    fn write(&mut self, address: u32, value: u8, clock: u64);
    /// What a read of `address` would return if it were made now, without its effects: the
    /// ROM byte there, for an instruction tried out ahead of time.
    fn peek(&self, address: u32) -> u8;
    /// The master clock from which the 68000's bus is free: the end of the last stall
    /// that held it (a DMA from 68000 memory, a full VDP FIFO).
    fn free_from(&self) -> u64;
    /// Whether the VDP asserts the Z80's interrupt input at `clock`.
    fn interrupt(&self, clock: u64) -> bool;
    /// Whether `address` is the PSG's port, for the sound log.
    fn is_psg(&self, address: u32) -> bool;
}

/// The Z80's state before an instruction it ran ahead of the 68000, to go back to.
#[derive(Debug, Clone)]
struct Mark {
    clock: u64,
    cpu: Core,
    bank: u16,
    ym: Control,
    /// The undo log's position then.
    undo: usize,
    sound: usize,
    counts: SoundCounts,
}

/// A write an instruction tried out ahead of time makes, applied when it is kept.
#[derive(Debug, Clone, Copy)]
enum Deferred {
    Ram(usize, u8),
    Ym2612 { offset: u32, value: u8, clock: u64 },
    Bank(u8),
}

/// The Z80, its RAM, the YM2612 and the bank register, and the two lines the 68000 drives.
#[derive(Debug)]
pub struct Z80 {
    cpu: Core,
    ram: Box<[u8; RAM_SIZE]>,
    bank: u16,
    ym: Control,
    requested: bool,
    reset: bool,
    /// Where the standing bus request was granted.
    granted: u64,
    /// The master clock the Z80 has run to.
    clock: u64,
    /// The instruction at `clock` reaches the 68000's bus, first at this master clock; it
    /// runs when the 68000 gets there.
    pending: Option<u64>,
    /// States before the instructions run ahead of the 68000, oldest first.
    journal: Vec<Mark>,
    /// RAM bytes as they were before each write made ahead: index, old value.
    undo: Vec<(usize, u8)>,
    /// The position of `undo[0]` among all undo entries ever made.
    undo_base: usize,
    /// The machine cycle starts of the last instruction run with the 68000's bus attached,
    /// then its end, in master clocks: where a bus request during it is granted.
    cycles: Vec<u64>,
    deferred: Vec<Deferred>,
    sound: SoundLog,
    /// The line was released at a time before the end of the current budget, which may now
    /// hold an event of the Z80's.
    rescheduled: bool,
}

impl Default for Z80 {
    /// Power-on: the Z80 in reset, the 68000 not asking for its bus.
    fn default() -> Self {
        Self {
            cpu: Core::default(),
            ram: Box::new([0; RAM_SIZE]),
            bank: 0,
            ym: Control::default(),
            requested: false,
            reset: true,
            granted: 0,
            clock: 0,
            pending: None,
            journal: Vec::new(),
            undo: Vec::new(),
            undo_base: 0,
            cycles: Vec::new(),
            deferred: Vec::new(),
            sound: SoundLog::default(),
            rescheduled: false,
        }
    }
}

impl Z80 {
    /// Z80 RAM, as far as the Z80 has run.
    pub fn ram(&self) -> &[u8; RAM_SIZE] {
        &self.ram
    }

    /// The Z80's registers, as far as it has run.
    pub fn cpu(&self) -> &Core {
        &self.cpu
    }

    /// The bank register: 68000 address bits 15-23 of the window at 0x8000.
    pub fn bank(&self) -> u16 {
        self.bank
    }

    /// The master clock the Z80 has run to; ahead of the 68000 while it runs.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Whether the 68000 has asked for the Z80's bus.
    pub fn bus_requested(&self) -> bool {
        self.requested
    }

    /// Whether the Z80's reset line is asserted.
    pub fn in_reset(&self) -> bool {
        self.reset
    }

    /// Whether the 68000 holds the Z80's bus at master clock `now`: asked for, granted by
    /// then, and the Z80 out of reset, which outranks the request.
    pub fn bus_held(&self, now: u64) -> bool {
        self.requested && !self.reset && now >= self.granted
    }

    /// Whether the Z80 executes: out of reset and not stopped for the 68000.
    fn running(&self) -> bool {
        !self.reset && !self.requested
    }

    pub(crate) fn sound(&self) -> &SoundLog {
        &self.sound
    }

    pub(crate) fn sound_mut(&mut self) -> &mut SoundLog {
        &mut self.sound
    }

    pub(crate) fn take_rescheduled(&mut self) -> bool {
        std::mem::take(&mut self.rescheduled)
    }

    /// Z80 RAM byte `index`, for the 68000 holding the bus.
    pub(crate) fn read(&self, index: usize) -> u8 {
        self.ram[index]
    }

    /// Writes Z80 RAM byte `index`, for the 68000 holding the bus.
    pub(crate) fn write(&mut self, index: usize, value: u8) {
        self.ram[index] = value;
    }

    /// The YM2612's status at `now`, for the 68000 holding the bus.
    pub(crate) fn ym_status(&self, now: u64) -> u8 {
        self.ym.status(now)
    }

    /// A YM2612 write by the 68000 holding the bus.
    pub(crate) fn ym_write(&mut self, offset: u32, value: u8, now: u64) -> Written {
        self.ym.write(offset, value, now)
    }

    /// A write to the bank register by the 68000 holding the bus.
    pub(crate) fn write_bank(&mut self, value: u8) {
        self.bank = shift_bank(self.bank, value);
    }

    /// Runs the Z80 up to the 68000's time `now`: the instructions that reach the 68000's
    /// bus at or before it run with it attached. Returns the 68000 cycles those accesses
    /// took from the 68000.
    pub(crate) fn sync(&mut self, now: u64, main: &mut impl Main) -> u32 {
        let mut steals = 0;
        while self.running() {
            match self.pending {
                Some(access) if access <= now => steals += self.execute(main),
                Some(_) => break,
                None if self.clock >= now => break,
                None => self.run_ahead(now, main),
            }
        }
        self.commit(now);
        steals
    }

    /// The master clock where the Z80 next reaches the 68000's bus, running ahead as far
    /// as `until` to find it; `None` if it does not before then, or does not run.
    pub(crate) fn next_event(&mut self, until: u64, main: &impl Main) -> Option<u64> {
        if !self.running() {
            return None;
        }
        self.run_ahead(until, main);
        self.pending
    }

    /// The 68000 sets the bus request line at `now`. Returns the 68000 cycles the Z80's
    /// accesses up to the grant took from it.
    pub(crate) fn request(&mut self, now: u64, main: &mut impl Main) -> u32 {
        if self.requested {
            return 0;
        }
        let mut steals = 0;
        self.granted = if self.running() {
            steals = self.sync(now, main);
            let grant = if self.rollback(now) && self.clock < now {
                steals += self.execute(main);
                self.cycle_end_after(now)
            } else if self.clock > now {
                // The instruction running at `now` ran with the 68000's bus attached.
                self.cycle_end_after(now)
            } else {
                now
            };
            self.forget();
            grant
        } else {
            now
        };
        self.requested = true;
        steals
    }

    /// The 68000 clears the bus request line at `now`.
    pub(crate) fn release(&mut self, now: u64) {
        if !self.requested {
            return;
        }
        self.requested = false;
        if !self.reset {
            self.resume(now);
        }
    }

    /// The 68000 asserts the reset line at `now`: the Z80 stops where it is, mid-instruction
    /// if need be, and the YM2612, which shares the line, is reset with it.
    pub(crate) fn assert_reset(&mut self, now: u64, main: &mut impl Main) -> u32 {
        if self.reset {
            return 0;
        }
        let mut steals = 0;
        if self.running() {
            steals = self.sync(now, main);
            self.rollback(now);
        }
        self.reset = true;
        self.cpu.reset();
        self.ym = Control::default();
        self.clock = self.clock.max(now);
        self.forget();
        self.sound.record(SoundWrite {
            clock: now,
            cpu: Cpu::M68000,
            chip: Chip::Ym2612Reset,
            value: 0,
        });
        steals
    }

    /// The 68000 releases the reset line at `now`: the Z80 starts from address 0, unless
    /// the bus is still requested, which is then granted.
    pub(crate) fn release_reset(&mut self, now: u64) {
        if !self.reset {
            return;
        }
        self.reset = false;
        if self.requested {
            self.granted = now;
        } else {
            self.resume(now);
        }
    }

    fn resume(&mut self, now: u64) {
        self.clock = self.clock.max(now);
        self.rescheduled = true;
    }

    /// Where the machine cycle holding `clock` ends, in the last instruction run with the
    /// 68000's bus attached.
    fn cycle_end_after(&self, clock: u64) -> u64 {
        self.cycles
            .iter()
            .copied()
            .find(|&start| start > clock)
            .unwrap_or(self.clock)
    }

    /// Tries the instructions from `clock` on, keeping each that stays on the Z80's own
    /// bus, until one reaches the 68000's bus (it becomes `pending`) or `until` is reached.
    fn run_ahead(&mut self, until: u64, main: &impl Main) {
        while self.running() && self.pending.is_none() && self.clock < until {
            let mut cpu = self.cpu.clone();
            cpu.set_interrupt_line(main.interrupt(self.clock));
            self.deferred.clear();
            let mut trial = Trial {
                ram: &self.ram,
                bank: self.bank,
                ym: &self.ym,
                main,
                start: self.clock,
                deferred: &mut self.deferred,
                main_at: None,
            };
            let t_states = cpu.step(&mut trial);
            if let Some(at) = trial.main_at {
                self.pending = Some(self.clock + u64::from(at) * CLOCK_DIVIDER);
                break;
            }
            self.journal.push(Mark {
                clock: self.clock,
                cpu: std::mem::replace(&mut self.cpu, cpu),
                bank: self.bank,
                ym: self.ym,
                undo: self.undo_base + self.undo.len(),
                sound: self.sound.position(),
                counts: self.sound.counts(),
            });
            for index in 0..self.deferred.len() {
                self.apply(self.deferred[index]);
            }
            self.clock += u64::from(t_states) * CLOCK_DIVIDER;
        }
    }

    /// Applies a write of an instruction kept from its trial.
    fn apply(&mut self, write: Deferred) {
        match write {
            Deferred::Ram(index, value) => {
                self.undo.push((index, self.ram[index]));
                self.ram[index] = value;
            }
            Deferred::Ym2612 {
                offset,
                value,
                clock,
            } => {
                let written = self.ym.write(offset, value, clock);
                record_ym(&mut self.sound, written, value, clock);
            }
            Deferred::Bank(value) => self.bank = shift_bank(self.bank, value),
        }
    }

    /// Runs the instruction at `clock` with the 68000's bus attached. Returns the 68000
    /// cycles its accesses there took from the 68000.
    fn execute(&mut self, main: &mut impl Main) -> u32 {
        let start = self.clock;
        self.cpu.set_interrupt_line(main.interrupt(start));
        self.cycles.clear();
        let Self {
            cpu,
            ram,
            bank,
            ym,
            cycles,
            sound,
            ..
        } = self;
        let mut attached = Attached {
            ram,
            bank,
            ym,
            sound,
            main,
            cycles,
            start,
            wait: 0,
            accesses: 0,
        };
        let t_states = cpu.step(&mut attached);
        let accesses = attached.accesses;
        self.clock = start + u64::from(t_states) * CLOCK_DIVIDER;
        self.cycles.push(self.clock);
        self.pending = None;
        self.forget();
        accesses * MAIN_STEAL
    }

    /// Goes back to the instruction running at `clock`, if the Z80 has run past it and it
    /// was run ahead. Returns whether the Z80 now stands at or before `clock`.
    fn rollback(&mut self, clock: u64) -> bool {
        self.pending = None;
        if self.clock <= clock {
            return true;
        }
        let index = self.journal.partition_point(|mark| mark.clock <= clock);
        if index == 0 {
            return false;
        }
        self.journal.truncate(index);
        let Some(mark) = self.journal.pop() else {
            return false;
        };
        let keep = mark.undo - self.undo_base;
        for &(at, old) in self.undo[keep..].iter().rev() {
            self.ram[at] = old;
        }
        self.undo.truncate(keep);
        self.cpu = mark.cpu;
        self.bank = mark.bank;
        self.ym = mark.ym;
        self.sound.truncate(mark.sound, mark.counts);
        self.clock = mark.clock;
        true
    }

    /// Drops what no rollback can go back to any more: the 68000 has reached `now`, and
    /// lines it moves later take the Z80 back no further than that.
    fn commit(&mut self, now: u64) {
        if self.clock <= now {
            self.forget();
            return;
        }
        let keep = self
            .journal
            .partition_point(|mark| mark.clock <= now)
            .saturating_sub(1);
        self.journal.drain(..keep);
        let oldest = self
            .journal
            .first()
            .map_or(self.undo_base + self.undo.len(), |mark| mark.undo);
        self.undo.drain(..oldest - self.undo_base);
        self.undo_base = oldest;
    }

    /// Forgets the journal: the Z80 cannot go back before where it stands.
    fn forget(&mut self) {
        self.journal.clear();
        self.undo_base += self.undo.len();
        self.undo.clear();
    }
}

/// Logs a YM2612 data write the Z80 made.
fn record_ym(sound: &mut SoundLog, written: Written, value: u8, clock: u64) {
    if written.timer {
        sound.timer(Cpu::Z80);
    }
    if let Some((part, register)) = written.data {
        sound.record(SoundWrite {
            clock,
            cpu: Cpu::Z80,
            chip: Chip::Ym2612 { part, register },
            value,
        });
    }
}

/// The bus for an instruction tried ahead of the 68000: reads see the Z80's side as it is
/// and the ROM through the window, writes wait in `deferred`, and the first access to the
/// 68000's bus is noted, which leaves the instruction for later.
struct Trial<'a, M: Main> {
    ram: &'a [u8; RAM_SIZE],
    bank: u16,
    ym: &'a Control,
    main: &'a M,
    start: u64,
    deferred: &'a mut Vec<Deferred>,
    main_at: Option<u32>,
}

impl<M: Main> CoreBus for Trial<'_, M> {
    fn read(&mut self, address: u16, at: u32) -> u8 {
        match Reach::of(address, self.bank) {
            Reach::Ram(index) => self.ram[index],
            Reach::Ym2612(_) => self.ym.status(self.start + u64::from(at) * CLOCK_DIVIDER),
            Reach::Bank | Reach::Nothing => OPEN,
            Reach::Main(target) => {
                self.main_at.get_or_insert(at);
                self.main.peek(target)
            }
        }
    }

    fn write(&mut self, address: u16, value: u8, at: u32) {
        let write = match Reach::of(address, self.bank) {
            Reach::Ram(index) => Deferred::Ram(index, value),
            Reach::Ym2612(offset) => Deferred::Ym2612 {
                offset,
                value,
                clock: self.start + u64::from(at) * CLOCK_DIVIDER,
            },
            Reach::Bank => Deferred::Bank(value),
            Reach::Nothing => return,
            Reach::Main(_) => {
                self.main_at.get_or_insert(at);
                return;
            }
        };
        self.deferred.push(write);
    }

    /// The Z80's I/O ports are not connected.
    fn input(&mut self, _port: u16, _at: u32) -> u8 {
        OPEN
    }

    fn output(&mut self, _port: u16, _value: u8, _at: u32) {}
}

/// The bus for an instruction run with the 68000's bus attached: everything takes effect
/// at once, and each access to the 68000's bus waits for it.
struct Attached<'a, M: Main> {
    ram: &'a mut [u8; RAM_SIZE],
    bank: &'a mut u16,
    ym: &'a mut Control,
    sound: &'a mut SoundLog,
    main: &'a mut M,
    cycles: &'a mut Vec<u64>,
    start: u64,
    /// Wait states the access just made adds.
    wait: u32,
    /// Accesses to the 68000's bus so far.
    accesses: u32,
}

impl<M: Main> Attached<'_, M> {
    fn clock(&self, at: u32) -> u64 {
        self.start + u64::from(at) * CLOCK_DIVIDER
    }

    /// Notes a machine cycle starting at T-state `at`.
    fn cycle(&mut self, at: u32) {
        let clock = self.clock(at);
        self.cycles.push(clock);
    }

    /// An access to the 68000's bus starting at `clock`: it waits for the bus to be free,
    /// then for the arbiter.
    fn main_access(&mut self, clock: u64) {
        self.accesses += 1;
        let held = self.main.free_from().saturating_sub(clock);
        // A stall is far shorter than u32::MAX T-states.
        self.wait = MAIN_WAIT + held.div_ceil(CLOCK_DIVIDER) as u32;
    }
}

impl<M: Main> CoreBus for Attached<'_, M> {
    fn read(&mut self, address: u16, at: u32) -> u8 {
        self.cycle(at - 1);
        match Reach::of(address, *self.bank) {
            Reach::Ram(index) => self.ram[index],
            Reach::Ym2612(_) => self.ym.status(self.clock(at)),
            Reach::Bank | Reach::Nothing => OPEN,
            Reach::Main(target) => {
                let clock = self.clock(at);
                self.main_access(clock);
                self.main.read(target, clock)
            }
        }
    }

    fn write(&mut self, address: u16, value: u8, at: u32) {
        self.cycle(at - 1);
        let clock = self.clock(at);
        match Reach::of(address, *self.bank) {
            Reach::Ram(index) => self.ram[index] = value,
            Reach::Ym2612(offset) => {
                let written = self.ym.write(offset, value, clock);
                record_ym(self.sound, written, value, clock);
            }
            Reach::Bank => *self.bank = shift_bank(*self.bank, value),
            Reach::Nothing => {}
            Reach::Main(target) => {
                self.main_access(clock);
                if self.main.is_psg(target) {
                    self.sound.record(SoundWrite {
                        clock,
                        cpu: Cpu::Z80,
                        chip: Chip::Psg,
                        value,
                    });
                }
                self.main.write(target, value, clock);
            }
        }
    }

    fn input(&mut self, _port: u16, at: u32) -> u8 {
        self.cycle(at - 2);
        OPEN
    }

    fn output(&mut self, _port: u16, _value: u8, at: u32) {
        self.cycle(at - 2);
    }

    fn wait(&mut self) -> u32 {
        std::mem::take(&mut self.wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 68000 side with a ROM, a log of the Z80's accesses there, and an interrupt line
    /// asserted over a fixed span.
    #[derive(Default)]
    struct Fake {
        rom: Vec<u8>,
        accesses: Vec<(u32, Option<u8>, u64)>,
        free: u64,
        interrupt: Option<(u64, u64)>,
    }

    impl Main for Fake {
        fn read(&mut self, address: u32, clock: u64) -> u8 {
            self.accesses.push((address, None, clock));
            self.peek(address)
        }

        fn write(&mut self, address: u32, value: u8, clock: u64) {
            self.accesses.push((address, Some(value), clock));
        }

        fn peek(&self, address: u32) -> u8 {
            self.rom.get(address as usize).copied().unwrap_or(OPEN)
        }

        fn free_from(&self) -> u64 {
            self.free
        }

        fn interrupt(&self, clock: u64) -> bool {
            self.interrupt
                .is_some_and(|(from, to)| (from..to).contains(&clock))
        }

        fn is_psg(&self, address: u32) -> bool {
            address == VDP_BASE | 0x11
        }
    }

    const T: u64 = CLOCK_DIVIDER;

    /// A running Z80 with `program` at address 0.
    fn running(program: &[u8]) -> Z80 {
        let mut z80 = Z80::default();
        z80.ram[..program.len()].copy_from_slice(program);
        z80.release_reset(0);
        z80
    }

    /// INC (HL) with HL = 0x1000, then JR back: 11 + 12 T a turn, one RAM write each.
    const COUNT_LOOP: [u8; 6] = [0x21, 0x00, 0x10, 0x34, 0x18, 0xFD];

    #[test]
    fn test_the_68000_sees_ram_twice_and_the_ym_bank_and_vdp_after() {
        assert_eq!(Target::of(0x0000), Target::Ram(0));
        assert_eq!(Target::of(0x2001), Target::Ram(1));
        assert_eq!(Target::of(0x3FFF), Target::Ram(0x1FFF));
        assert_eq!(Target::of(0x4000), Target::Ym2612(0));
        assert_eq!(Target::of(0x5FFF), Target::Ym2612(0x1FFF));
        assert_eq!(Target::of(0x6000), Target::Bank);
        assert_eq!(Target::of(0x6100), Target::Unmapped);
        assert_eq!(Target::of(0x7F11), Target::Hang);
        assert_eq!(Target::of(0x8000), Target::Ram(0));
        assert_eq!(Target::of(0xC000), Target::Ym2612(0));
    }

    #[test]
    fn test_bank_register_takes_the_first_bit_into_a15() {
        // Nine writes, first to last: A15 = 1, A16-A22 = 0, A23 = 1.
        let bank = [1, 0, 0, 0, 0, 0, 0, 0, 1].into_iter().fold(0, shift_bank);
        assert_eq!(bank, 0x101);
        assert_eq!(Reach::of(0x8123, bank), Reach::Main(0x80_8123));
        assert_eq!(Reach::of(0x7F11, bank), Reach::Main(0xC0_0011));
        assert_eq!(Reach::of(0x2005, bank), Reach::Ram(5));
        assert_eq!(Reach::of(0x6100, bank), Reach::Nothing);
    }

    #[test]
    fn test_run_ahead_stops_at_the_first_access_to_the_68000_bus() {
        // NOP; LD A,(0x8002), from bank 0: the ROM byte at 2.
        let mut z80 = running(&[0x00, 0x3A, 0x02, 0x80]);
        let mut main = Fake {
            rom: vec![0, 0, 0x5A],
            ..Fake::default()
        };
        // The read strobe falls 11 T-states into the instruction, in its fourth cycle.
        let access = 4 * T + 11 * T;
        assert_eq!(z80.next_event(u64::MAX, &main), Some(access));
        assert!(
            main.accesses.is_empty(),
            "not run before the 68000 gets there"
        );
        assert_eq!(z80.clock(), 4 * T);

        let steals = z80.sync(access, &mut main);
        assert_eq!(steals, MAIN_STEAL);
        assert_eq!(z80.cpu().a, 0x5A);
        assert_eq!(main.accesses, [(2, None, access)]);
        // 13 T plus the wait.
        assert_eq!(z80.clock(), (4 + 13 + u64::from(MAIN_WAIT)) * T);
    }

    #[test]
    fn test_a_bank_access_waits_out_a_stall_of_the_68000_bus() {
        let mut z80 = running(&[0x3A, 0x00, 0x80]);
        let mut main = Fake {
            rom: vec![0x11],
            free: 1_000,
            ..Fake::default()
        };
        z80.sync(1_000, &mut main);
        let access = 11 * T;
        let held = (1_000 - access).div_ceil(T);
        assert_eq!(z80.clock(), (13 + u64::from(MAIN_WAIT) + held) * T);
    }

    #[test]
    fn test_psg_writes_through_the_vdp_area_are_logged() {
        // LD A,0x9F; LD (0x7F11),A
        let mut z80 = running(&[0x3E, 0x9F, 0x32, 0x11, 0x7F]);
        let mut main = Fake::default();
        z80.sync(1_000, &mut main);
        assert_eq!(
            main.accesses.first(),
            Some(&(0xC0_0011, Some(0x9F), 18 * T))
        );
        assert_eq!(z80.sound().counts().psg, [0, 1]);
    }

    #[test]
    fn test_a_late_bus_request_takes_back_what_ran_ahead() {
        let mut ahead = running(&COUNT_LOOP);
        let mut main = Fake::default();
        assert_eq!(ahead.next_event(100_000, &main), None);
        assert!(ahead.ram[0x1000] > 10);

        // Request in the middle of the fourth INC (HL), 10 + 23 * 3 + 5 T-states in.
        let now = (10 + 23 * 3 + 5) * T;
        ahead.request(now, &mut main);
        let mut straight = running(&COUNT_LOOP);
        straight.request(now, &mut main);
        assert_eq!(ahead.ram[0x1000], 4);
        assert_eq!(ahead.ram[..], straight.ram[..]);
        assert_eq!(ahead.cpu(), straight.cpu());
        assert_eq!(ahead.clock(), straight.clock());

        // Granted where INC (HL)'s read cycle ends (its cycles start at T 0, 4, 8), not
        // at the end of the instruction: T 8 from the instruction's start at 10 + 69.
        let grant = (10 + 23 * 3 + 8) * T;
        assert!(!ahead.bus_held(grant - 1));
        assert!(ahead.bus_held(grant));
        assert_eq!(ahead.clock(), (10 + 23 * 3 + 11) * T);
    }

    #[test]
    fn test_the_z80_waits_while_the_bus_is_held_and_resumes_from_the_release() {
        let mut z80 = running(&COUNT_LOOP);
        let mut main = Fake::default();
        z80.request(0, &mut main);
        assert!(z80.bus_held(0));
        z80.sync(50_000, &mut main);
        assert_eq!(z80.ram[0x1000], 0);
        assert_eq!(z80.next_event(u64::MAX, &main), None);
        z80.release(50_000);
        assert!(z80.take_rescheduled());
        z80.sync(50_000 + 33 * T, &mut main);
        assert_eq!(z80.ram[0x1000], 1);
    }

    #[test]
    fn test_reset_outranks_the_request_and_stops_mid_instruction() {
        let mut z80 = running(&COUNT_LOOP);
        let mut main = Fake::default();
        z80.sync(10 * T + 2 * 23 * T, &mut main);
        z80.assert_reset(10 * T + 2 * 23 * T + 3 * T, &mut main);
        assert_eq!(z80.ram[0x1000], 2, "the third INC (HL) never wrote");
        assert_eq!(z80.cpu().pc, 0);
        z80.request(50_000, &mut main);
        assert!(!z80.bus_held(60_000), "no grant while in reset");
        z80.release_reset(70_000);
        assert!(z80.bus_held(70_000));
        z80.release(80_000);
        z80.sync(80_000 + 10 * T, &mut main);
        assert_eq!(z80.cpu().pc, 3, "started again from 0");
    }

    #[test]
    fn test_reset_clears_the_ym2612_busy_flag() {
        // LD A,0x2A; LD (0x4000),A; LD (0x4001),A
        let mut z80 = running(&[0x3E, 0x2A, 0x32, 0x00, 0x40, 0x32, 0x01, 0x40]);
        let mut main = Fake::default();
        z80.sync(40 * T, &mut main);
        assert_eq!(z80.sound().counts().dac, [0, 1]);
        assert_ne!(z80.ym_status(40 * T), 0);
        z80.assert_reset(40 * T, &mut main);
        assert_eq!(z80.ym_status(40 * T), 0);
    }

    #[test]
    fn test_ym_writes_made_ahead_are_taken_back_with_the_z80() {
        // LD A,0x2A; LD (0x4000),A; then forever: LD (0x4001),A
        let program = [0x3E, 0x2A, 0x32, 0x00, 0x40, 0x32, 0x01, 0x40, 0x18, 0xFB];
        let mut z80 = running(&program);
        let mut main = Fake::default();
        z80.sound_mut().set_recording(true);
        z80.next_event(10_000, &main);
        let ahead = z80.sound().counts().dac[1];
        assert!(ahead > 5);
        // During the JR after the first sample (T-states 33-45), before the second.
        z80.request(40 * T, &mut main);
        assert_eq!(z80.sound().counts().dac, [0, 1]);
        assert_eq!(z80.sound_mut().take_before(u64::MAX).len(), 1);
    }

    #[test]
    fn test_the_vdp_interrupt_runs_the_im1_handler_once_per_assertion() {
        // 0x00: LD SP,0x2000; LD HL,0x1000; IM 1; EI; then JR to itself. 0x38: INC (HL),
        // a delay longer than the line, so the line has dropped when it returns, EI; RET.
        let mut program = vec![
            0x31, 0x00, 0x20, 0x21, 0x00, 0x10, 0xED, 0x56, 0xFB, 0x18, 0xFE,
        ];
        program.resize(0x38, 0);
        // INC (HL); LD B,0; DJNZ to itself (256 turns, about 3,300 T); EI; RET
        program.extend([0x34, 0x06, 0x00, 0x10, 0xFE, 0xFB, 0xC9]);
        let mut z80 = running(&program);
        let mut main = Fake {
            interrupt: Some((1_000, 1_000 + 3420)),
            ..Fake::default()
        };
        z80.sync(200_000, &mut main);
        assert_eq!(z80.ram[0x1000], 1);
    }
}
