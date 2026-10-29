//! Running translated code: the top-level loop, the dispatcher with its stepped-code
//! fallback, the scheduler that keeps time and delivers interrupts, the run limits and the
//! native-depth guard.
//!
//! Every translated function returns the address execution continues at, and every caller
//! compares it with the return address it expects. The runtime uses that one convention for
//! all of its own control:
//!
//! - A return no native frame expects cascades to [`Runtime::run`], which continues at the
//!   returned address through `ag_table`, else the stepped function there.
//! - Stepped code (one native function per instruction, see the codegen crate) hands control
//!   to the translated function at the first table row it reaches. An address with neither
//!   ends the run with [`Exit::NoNativeCode`]; with the `interpreter` feature the IR
//!   interpreter runs it instead, up to the first table row, as a dispatcher miss.
//! - An interrupt handler is called natively from a check point with its frame pushed, and
//!   returns the stacked PC through RTE. The check point continues when that is its own
//!   address and returns anything else, so a handler that rewrote the stacked PC goes
//!   through the dispatcher.
//! - [`UNWIND`] marks a return value no return check can match (they compare 24-bit
//!   addresses): it carries a pc in its low bits down to the top level, which resumes there
//!   with an empty native stack or ends the run.
//!
//! Time: translated code counts `AgState::cycles` down from a budget the scheduler sets.
//! The scheduler keeps the master clock ([`Devices`]); a budget ends only where something
//! can raise an enabled interrupt ([`Hardware::next_interrupt`]), at the end of a frame
//! while [`Runtime::run_frames`] waits for one, and at the cycle cap, a slice at most. A
//! register write that moves the next interrupt earlier cuts the budget there. Refresh
//! ([`crate::refresh`]) is charged per budget on the cycles executed, and a budget ends
//! where those and their refresh first reach the event.
//!
//! Interrupts are taken at instruction boundaries (the M0.2 hybrid model). Each check point
//! knows the most cycles the rest of its block can take, at block starts and wherever a
//! call, an exception handler or STOP came back inside the block: when the budget may end
//! before the block does, the block goes on as stepped code, which calls the scheduler at
//! the boundary where the budget runs out, and hands back at the next table row. After an
//! instruction that may reach a device, a stall or an interrupt its access made due sends
//! the rest of the block to stepped code the same way. A write to SR (MOVE, ANDI, ORI, EORI
//! to SR, RTE) calls `ag_deliver`, which takes an interrupt the new mask lets through before
//! the next instruction; an interrupt waiting behind the mask therefore leaves the budget
//! alone. A native check point that finds the budget spent by the cycles run (not cut short
//! to take an interrupt there) took it past its boundary: it is counted as late
//! ([`Runtime::late_interrupts`]), which no translated program should show.
//!
//! An instruction acts on the interrupt level it latched partway through
//! (`Lifted::latch`): an interrupt that becomes pending after that point waits for the end of
//! the next instruction, which stepped code (or the interpreter) runs as a one-instruction
//! budget. One already pending when SR unmasks it is taken at once. Taking an interrupt
//! costs 49-58 cycles, its acknowledge waiting for E (see [`ACKNOWLEDGE_LEAST`]); the
//! program's `ag_interrupt` pushes its frame.

#[cfg(feature = "interpreter")]
use std::collections::{HashMap, HashSet};
use std::{any::Any, fmt, ptr, slice};

#[cfg(feature = "interpreter")]
use anygenesis_abi::Region;
use anygenesis_abi::{AgEntry, AgFn, AgInterrupt, AgState, RAM_SIZE};
#[cfg(feature = "interpreter")]
use anygenesis_ir::{
    Flag, Lifted,
    interp::{Cpu, Interpreter, Step, Transfer},
    interrupt_entry, lift,
};
use anygenesis_rom::Rom;
#[cfg(feature = "interpreter")]
use anygenesis_shared::Address;

use crate::{
    bus::{Devices, SystemBus},
    hardware::Hardware,
    io::{Buttons, VERSION_NO_EXPANSION, VERSION_OVERSEAS},
    log::Log,
    refresh::Refresh,
    sound::{SoundCounts, SoundWrite},
    timing::CPU_DIVIDER,
    z80,
};

/// Set in a return value that must reach the top level; the low 24 bits hold a pc.
pub const UNWIND: u32 = 0x8000_0000;

/// `latch` from a check point whose slow path follows no instruction's latch rule
/// (`AG_NO_LATCH` in the generated C).
const NO_LATCH: u32 = 0xFFFF;

/// `ag_step_end` kinds (`AG_STEP_GO`, `AG_STEP_CALL` in the generated C; any other is
/// `AG_STEP_STOP`).
pub(crate) const STEP_GO: u32 = 0;
const STEP_CALL: u32 = 1;

/// A thread stack with room for the native-depth guard's default limits.
pub const RUN_STACK: usize = 64 << 20;

const ADDRESS_MASK: u32 = 0xFF_FFFF;

/// The status register after reset: supervisor mode, every interrupt masked.
const RESET_SR: u16 = 0x2700;

/// Cycles the 68000 spends taking an interrupt with a 4-cycle acknowledge (MC68000 User's
/// Manual, Table 8-14).
const INTERRUPT_CYCLES: i32 = 44;

/// The console answers every interrupt acknowledge with VPA, so the acknowledge is a 6800
/// cycle synchronised to E, the 68000 clock divided by [`E_PERIOD`]: 9 cycles plus E's
/// phase where it starts, [`ACKNOWLEDGE_START`] cycles into the sequence, instead of 4
/// (measured 9-18 on hardware, SpritesMind forum, 2015; BlastEm takes the same).
const ACKNOWLEDGE_LEAST: i32 = 9;
const ACKNOWLEDGE_NOMINAL: i32 = 4;
const ACKNOWLEDGE_START: u64 = 10;
const E_PERIOD: u64 = 10;

/// The level no mask holds back.
const NON_MASKABLE: u8 = 7;

/// Settings for a run.
#[derive(Debug, Clone)]
pub struct Options {
    /// The version register: region, video standard, expansion unit, hardware version.
    pub version: u8,
    /// What reads of unmapped addresses return.
    pub open_bus: u16,
    /// The most cycles code runs between calls of the check-point hook.
    pub slice: i32,
    /// The run ends once this many cycles have been charged.
    pub max_cycles: u64,
    /// End the run at an idle loop no interrupt can end.
    pub stop_when_idle: bool,
    /// Native stack depth, in bytes, past which the guard unwinds to the top level.
    pub soft_depth: usize,
    /// Native stack depth past which the process aborts; keep it below the run's stack.
    pub hard_depth: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            // An overseas NTSC console with TMSS (hardware version 1), no Mega CD.
            version: VERSION_OVERSEAS | VERSION_NO_EXPANSION | 1,
            open_bus: 0,
            // One NTSC frame, rounded down: budgets end at events, not at slices.
            slice: 128_005,
            max_cycles: u64::MAX,
            stop_when_idle: false,
            soft_depth: 8 << 20,
            hard_depth: 56 << 20,
        }
    }
}

/// Why [`Runtime::run`] returned. The state is left at `pc`, so running again continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Exit {
    /// A check point came round again at `pc` with registers and RAM unchanged and no device
    /// touched, and no enabled interrupt is above the mask: a wait nothing can end.
    Idle { pc: u32 },
    /// STOP, continuing at `resume`, with no enabled interrupt above the mask it set.
    Stopped { resume: u32 },
    /// `Options::max_cycles` ran out at `pc`.
    CycleCap { pc: u32 },
    /// The frames [`Runtime::run_frames`] asked for have ended; the vertical interrupt that
    /// ended the last one is delivered when the run continues.
    Frames { pc: u32 },
    /// Execution reached `pc`, which has no native code (code in RAM, or an address the
    /// superset dropped), in a runtime without the interpreter. The run cannot continue.
    NoNativeCode { pc: u32 },
}

impl Exit {
    /// The address execution continues at.
    pub fn pc(self) -> u32 {
        match self {
            Self::Idle { pc }
            | Self::Stopped { resume: pc }
            | Self::CycleCap { pc }
            | Self::Frames { pc }
            | Self::NoNativeCode { pc } => pc,
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle { pc } => write!(f, "idle loop at ${pc:06X}"),
            Self::Stopped { resume } => write!(f, "STOP for good, resuming at ${resume:06X}"),
            Self::CycleCap { pc } => write!(f, "cycle cap at ${pc:06X}"),
            Self::Frames { pc } => write!(f, "frame limit at ${pc:06X}"),
            Self::NoNativeCode { pc } => write!(f, "no native code at ${pc:06X}"),
        }
    }
}

/// The machine state of the previous check point, for idle detection.
#[derive(Debug)]
struct Snapshot {
    pc: u32,
    /// D0-D7, A0-A7, the inactive stack pointer and the system byte of SR.
    registers: [u32; 18],
    flags: [u8; 5],
    /// Device accesses made so far: a loop polling a device is waiting on it, not idle.
    accesses: u64,
    /// Work RAM at the previous check, valid only when registers and pc matched there too.
    ram: Option<Box<[u8; RAM_SIZE]>>,
}

impl Snapshot {
    fn registers(state: &AgState) -> ([u32; 18], [u8; 5]) {
        let mut registers = [0; 18];
        registers[..8].copy_from_slice(&state.d);
        registers[8..16].copy_from_slice(&state.a);
        registers[16] = state.inactive_sp;
        registers[17] = u32::from(state.sr);
        let flags = [state.x, state.n, state.z, state.v, state.c];
        (registers, flags)
    }

    /// Records this check and returns whether nothing changed since the previous one. RAM is
    /// copied only once pc and registers repeat, so busy code pays almost nothing.
    fn idle(previous: &mut Option<Self>, pc: u32, state: &AgState, accesses: u64) -> bool {
        let (registers, flags) = Self::registers(state);
        match previous {
            Some(snapshot)
                if snapshot.pc == pc
                    && snapshot.registers == registers
                    && snapshot.flags == flags
                    && snapshot.accesses == accesses =>
            {
                match &mut snapshot.ram {
                    Some(ram) if **ram == state.ram => true,
                    Some(ram) => {
                        ram.copy_from_slice(&state.ram);
                        false
                    }
                    None => {
                        snapshot.ram = Some(Box::new(state.ram));
                        false
                    }
                }
            }
            _ => {
                *previous = Some(Self {
                    pc,
                    registers,
                    flags,
                    accesses,
                    ram: None,
                });
                false
            }
        }
    }
}

/// What the `ag_*` externs reach through `AgState::runtime` while a run is in progress.
#[derive(Debug)]
pub(crate) struct Context {
    pub devices: Devices,
    table: &'static [AgEntry],
    /// The program's stepped function of each even ROM address, index address / 2.
    steps: &'static [Option<AgFn>],
    /// The program's checked entry into its stepped functions, `ag_step_entry`.
    step_entry: Option<AgFn>,
    /// The program's interrupt entry; without it the interpreter runs the IR's.
    interrupt_entry: Option<AgInterrupt>,
    options: Options,
    #[cfg(feature = "interpreter")]
    interpreter: Interpreter,
    /// Lifted instructions in ROM by address; code in RAM is decoded every time it runs.
    #[cfg(feature = "interpreter")]
    lifts: HashMap<u32, Lifted>,
    /// The native stack position of the top-level loop.
    stack_base: usize,
    /// Set when the run must end; the top level returns it.
    exit: Option<Exit>,
    /// Cycles charged so far.
    cycles: u64,
    /// The cycle count the current budget started from; `AgState::cycles` counts down
    /// from it.
    budget: i32,
    /// Cycles of the current budget the 68000 did not execute: device stalls and waiting
    /// in STOP, which cause no refresh.
    waited: u32,
    refresh: Refresh,
    /// The address after a STOP the CPU is waiting in.
    stopped: Option<u32>,
    /// The frame count at which [`Runtime::run_frames`] ends the run.
    frame_limit: Option<u64>,
    /// Interrupts taken, by level.
    interrupts: [u64; 8],
    /// Interrupts taken at a native check point after an instruction boundary had passed.
    late: u64,
    /// The budget was cut short so that the next check point takes an interrupt due there,
    /// rather than spent by the cycles run.
    cut: bool,
    /// Check points whose slow path ran the rest of their block as stepped code.
    slow_paths: u64,
    /// A stepped instruction begun with `ag_step_begin`, or the interrupt entry, is running:
    /// its accesses all see its start, and its end takes their stalls.
    stepping: bool,
    /// What the latch rule needs of the stepped instruction running.
    step: NativeAccess,
    /// The interrupt due at the next check arrived after the instruction before it latched
    /// the interrupt level, so the instruction after it takes it.
    latched_late: bool,
    /// The next budget lasts one instruction, at whose end the interrupt is taken.
    one_more: bool,
    /// The translated instruction whose device access came last, for `ag_step_enter`.
    access: NativeAccess,
    /// The last run ended at a limit inside a check, before its interrupts and next budget:
    /// the next run starts with that check, native (`true`) or stepped code's.
    recheck: Option<bool>,
    /// Addresses between table rows where execution stopped and may go on from the top
    /// level: where an interrupt was taken (its handler's RTE returns there) or a run
    /// ended. Without stepped code there, they resume through the interpreter without
    /// counting a miss.
    #[cfg(feature = "interpreter")]
    between: HashSet<u32>,
    idle: Option<Snapshot>,
    hook: Option<InterruptHook>,
}

/// What the interpreter's latch rule needs of an instruction: for a translated one that
/// reached a device, noted at its first access (every access of an instruction sees its
/// start, so a later one of the same instruction comes at that start plus the stalls
/// since); for a stepped one, at its start.
#[derive(Debug, Clone, Copy, Default)]
struct NativeAccess {
    /// The master clock at the instruction's start.
    began: u64,
    /// The next enabled interrupt event then.
    event: Option<u64>,
    /// Whether an interrupt was due already then.
    waiting: bool,
    /// Where a further access of the same instruction would come: the start plus its stalls.
    at: u64,
}

/// A function [`Runtime::set_interrupt_hook`] calls at every entry of one level's handler.
struct InterruptHook {
    level: u8,
    function: Box<dyn FnMut(&mut InterruptStop<'_>)>,
}

impl fmt::Debug for InterruptHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InterruptHook")
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

/// The machine at the entry of an interrupt handler, as an interrupt hook sees it: the
/// exception frame is pushed and the handler has not started. The hook's work takes no
/// emulated time.
#[derive(Debug)]
pub struct InterruptStop<'a> {
    level: u8,
    handler: u32,
    taken: u64,
    state: &'a AgState,
    devices: &'a mut Devices,
}

impl InterruptStop<'_> {
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The handler's address, where execution continues.
    pub fn handler(&self) -> u32 {
        self.handler
    }

    /// Interrupts taken at this level so far, this one included.
    pub fn taken(&self) -> u64 {
        self.taken
    }

    pub fn state(&self) -> &AgState {
        self.state
    }

    /// The hardware, if it is a `T`.
    pub fn hardware<T: Hardware>(&self) -> Option<&T> {
        (self.devices.hardware.as_ref() as &dyn Any).downcast_ref()
    }

    /// Sets the buttons held on the pad in port 1, from this point of the run on.
    pub fn set_pad(&mut self, buttons: Buttons) {
        self.devices.io.set_pad(buttons);
    }
}

impl Context {
    fn row(&self, address: u32) -> Option<AgFn> {
        self.table
            .binary_search_by_key(&address, |entry| entry.address)
            .ok()
            .map(|index| self.table[index].function)
    }

    /// Cycles used of the current budget, without refresh.
    fn elapsed(&self, state: &AgState) -> u32 {
        let used = i64::from(self.budget) - i64::from(state.cycles);
        u32::try_from(used.max(0)).unwrap_or(u32::MAX)
    }

    /// The time into the current budget the hardware sees: the cycles used and the refresh
    /// they caused so far.
    fn clock(&self, state: &AgState) -> u32 {
        let elapsed = self.elapsed(state);
        elapsed + self.refresh.pending(elapsed.saturating_sub(self.waited))
    }

    /// The bus for one access or instruction.
    ///
    /// # Safety
    /// `state.rom` must point at `state.rom_size` readable bytes, as [`Runtime`] sets them.
    pub unsafe fn bus<'a>(&'a mut self, state: &'a mut AgState) -> SystemBus<'a> {
        let elapsed = self.clock(state);
        SystemBus {
            // SAFETY: forwarded from the caller.
            rom: unsafe { rom(state) },
            ram: &mut state.ram,
            devices: &mut self.devices,
            elapsed,
        }
    }

    /// Returns how deep the native stack is below the top-level loop. Past the hard limit
    /// nothing can unwind safely any more, so the process ends there.
    fn depth(&self, address: u32) -> usize {
        let depth = self.stack_base.saturating_sub(stack_position());
        if depth > self.options.hard_depth {
            eprintln!(
                "runtime: native stack {depth} bytes deep at ${address:06X}, past the hard \
                 limit of {} bytes",
                self.options.hard_depth
            );
            std::process::abort();
        }
        depth
    }

    fn too_deep(&mut self, address: u32) -> bool {
        let deep = self.depth(address) > self.options.soft_depth;
        if deep {
            self.devices.log.unwind();
        }
        deep
    }

    /// Whether an interrupt the hardware asserts gets past `mask`.
    fn due(&self, mask: u8) -> Option<u8> {
        let level = self.devices.hardware.pending_interrupt()?;
        (level > mask || level == NON_MASKABLE).then_some(level)
    }

    /// Whether no interrupt the hardware may raise gets past `mask`: a wait for one would
    /// never end.
    fn waits_forever(&self, mask: u8) -> bool {
        let shift = (mask + 1).min(NON_MASKABLE);
        self.devices.hardware.enabled_interrupts() >> shift == 0
    }

    /// Makes the next check point call the hook. The budget shrinks by what is left of it,
    /// so the cycles used so far still count.
    fn check_soon(&mut self, state: &mut AgState) {
        let left = state.cycles.max(0);
        self.budget -= left;
        state.cycles -= left;
        self.cut = true;
    }

    /// Before a bus extern: notes the instruction making it, unless it is the one the last
    /// access came from.
    pub fn before_access(&mut self, state: &AgState) {
        let now = self.devices.clock_at(self.clock(state));
        if now != self.access.at {
            self.access = self.note(state, now);
        }
    }

    /// After a bus extern: a read or write cannot unwind or take an interrupt, so a deep
    /// stack or an interrupt the access made due makes the next check point call the hook,
    /// which can.
    pub fn after_access(&mut self, state: &mut AgState, address: u32) {
        self.charge_stall(state);
        if self.devices.take_rescheduled() {
            self.reschedule(state);
        }
        if self.depth(address) > self.options.soft_depth || self.due(mask(state.sr)).is_some() {
            self.check_soon(state);
        }
        self.access.at = self.devices.clock_at(self.clock(state));
    }

    /// The interpreter's latch rule for the translated instruction that ended at the current
    /// count, whose interrupt latch point lies `latch` cycles after its start: an interrupt
    /// whose event came after that point, during the instruction (its stalls included),
    /// waits for the end of the next one.
    fn latch_native(&mut self, state: &AgState, latch: u32) {
        let access = self.access;
        let latched = access.began + u64::from(latch) * CPU_DIVIDER;
        let ended = self.devices.clock_at(self.clock(state));
        if !access.waiting
            && access
                .event
                .is_some_and(|event| (latched + 1..=ended).contains(&event))
        {
            self.latched_late = true;
        }
    }

    /// An access moved the hardware's next interrupt, or let the Z80 run: the budget ends
    /// there if that is sooner, the cycles used so far still counting.
    fn reschedule(&mut self, state: &mut AgState) {
        // From the time the count stands at, which is past the access: it holds the rest of
        // the instruction (or, in translated code, of its segment) already.
        let now = self.devices.clock_at(self.clock(state));
        let horizon = self.horizon(now, state).unsigned_abs();
        let done = self.elapsed(state).saturating_sub(self.waited);
        // A horizon is at most a slice, so it fits.
        let left = self.refresh.until(done, horizon) as i32;
        let cut = state.cycles - left;
        if cut > 0 {
            self.budget -= cut;
            state.cycles -= cut;
        }
    }

    /// 68000 cycles from master clock `from` to the next point a budget must end at: where
    /// an enabled interrupt may become pending, the end of a frame while a frame limit is
    /// set, and where the Z80 next reaches the 68000's bus (it runs ahead to find it); a
    /// slice at most, at least 1.
    fn horizon(&mut self, from: u64, state: &mut AgState) -> i32 {
        let slice = u64::from(self.options.slice.max(1).unsigned_abs());
        let hardware = &self.devices.hardware;
        let frame = self.frame_limit.map(|_| hardware.next_frame());
        let cap = hardware
            .next_interrupt()
            .into_iter()
            .chain(frame)
            .min()
            .unwrap_or(u64::MAX)
            .min(from + slice * CPU_DIVIDER);
        let (rom, ram) = memory(state);
        let event = self
            .devices
            .z80_event(rom, ram, cap)
            .map_or(cap, |z80| z80.min(cap));
        let until = event.saturating_sub(from).div_ceil(CPU_DIVIDER);
        // At most the slice, so it fits.
        until.clamp(1, slice) as i32
    }

    /// Takes the cycles a device froze the 68000 for out of the budget, as if the access
    /// that caused them had taken that long.
    fn charge_stall(&mut self, state: &mut AgState) {
        let stall = self.devices.take_stall();
        self.waited = self.waited.saturating_add(stall.unsigned_abs());
        state.cycles = state.cycles.saturating_sub(stall);
    }

    /// Ends the current budget: the cycles used and the refresh they caused go to the run's
    /// total and to the hardware.
    fn charge(&mut self, state: &mut AgState) {
        self.charge_stall(state);
        let elapsed = self.elapsed(state);
        let executed = elapsed.saturating_sub(std::mem::take(&mut self.waited));
        let used = elapsed + self.refresh.charge(executed);
        self.cycles += u64::from(used);
        self.devices.settle(used);
        let (rom, ram) = memory(state);
        self.devices.sync_z80(rom, ram);
        self.budget = 0;
        state.cycles = 0;
    }

    /// Starts a budget that lasts until the [`Context::horizon`] or the cycle cap, less the
    /// refresh it will cause; none while an interrupt is due, which the next check point
    /// then takes, or one instruction when that is the one to take it. Cycles the Z80 took
    /// from the 68000 where the last budget ended come first.
    fn refill(&mut self, state: &mut AgState) {
        self.cut = false;
        let one_more = std::mem::take(&mut self.one_more);
        self.budget = if one_more {
            1
        } else if self.due(mask(state.sr)).is_some() {
            self.cut = true;
            0
        } else {
            let left = self.options.max_cycles.saturating_sub(self.cycles);
            let cap = i32::try_from(left).unwrap_or(i32::MAX).max(1);
            let horizon = self.horizon(self.devices.clock_at(0), state).min(cap);
            // At most the horizon, so it fits.
            self.refresh.until(0, horizon.unsigned_abs()) as i32
        };
        state.cycles = self.budget;
        if one_more {
            // The instruction still runs to its end: the stall comes on top of it.
            let stall = self.devices.take_stall();
            self.budget += stall;
            self.waited = self.waited.saturating_add(stall.unsigned_abs());
        } else {
            self.charge_stall(state);
        }
    }

    /// The limit the run has reached at `pc`, if any.
    fn limit(&self, pc: u32) -> Option<Exit> {
        if self.cycles >= self.options.max_cycles {
            return Some(Exit::CycleCap { pc });
        }
        self.frame_limit
            .is_some_and(|limit| self.devices.hardware.frames() >= limit)
            .then_some(Exit::Frames { pc })
    }

    /// Whether the run waits at `pc` for something that can never happen.
    fn hangs(&mut self, state: &AgState, pc: u32) -> bool {
        self.options.stop_when_idle
            && Snapshot::idle(&mut self.idle, pc, state, self.devices.accesses)
            && self.waits_forever(mask(state.sr))
    }

    /// Ends the run at `pc`. The interpreter can end it between table rows; the next run
    /// goes on there through the interpreter.
    fn end(&mut self, exit: Exit) -> u32 {
        self.exit = Some(exit);
        let pc = exit.pc();
        self.stopped_between(pc);
        UNWIND | pc
    }

    /// Notes that execution stopped at `pc`, which may lie between table rows.
    fn stopped_between(&mut self, pc: u32) {
        #[cfg(feature = "interpreter")]
        if self.row(pc).is_none() {
            self.between.insert(pc);
        }
        #[cfg(not(feature = "interpreter"))]
        let _ = pc;
    }

    /// Starts taking the interrupt at `level` before the instruction at `pc`: acknowledges
    /// and counts it. Its entry sequence comes next, then [`Context::entered_interrupt`].
    fn begin_interrupt(&mut self, level: u8, pc: u32) {
        self.latched_late = false;
        self.one_more = false;
        self.stopped_between(pc);
        self.devices.hardware.acknowledge(level);
        self.interrupts[usize::from(level)] += 1;
    }

    /// After the entry sequence: starts a budget with the exception's own cycles already
    /// used.
    fn entered_interrupt(&mut self, state: &mut AgState) {
        self.refill(state);
        state.cycles -= self.interrupt_cycles();
    }

    /// Runs the IR's interrupt entry sequence for `level` before `pc` through the
    /// interpreter, for runtimes given no `ag_interrupt`, and returns the handler's address.
    ///
    /// # Safety
    /// As for [`Context::bus`].
    #[cfg(feature = "interpreter")]
    unsafe fn interpret_interrupt_entry(&mut self, state: &mut AgState, level: u8, pc: u32) -> u32 {
        let mut cpu = load_cpu(state, pc);
        let entry = interrupt_entry(level, Address::new(pc));
        // SAFETY: forwarded from the caller.
        let rom = unsafe { rom(state) };
        let elapsed = self.clock(state);
        let Self {
            devices,
            interpreter,
            ..
        } = self;
        let mut bus = SystemBus {
            ram: &mut state.ram,
            rom,
            devices,
            elapsed,
        };
        let outcome = interpreter.execute(&entry, &mut cpu, &mut bus);
        store_cpu(&cpu, state);
        match outcome.transfer {
            Some(Transfer::Jump(handler)) => handler.get(),
            // The entry sequence always ends in a jump through the vector table.
            _ => unreachable!("an interrupt entry leaves through its vector"),
        }
    }

    /// Cycles taking an interrupt that starts at the current budget's start: the acknowledge
    /// waits for E, whose phase counts from power-on.
    fn interrupt_cycles(&self) -> i32 {
        let acknowledge = self.devices.clock_at(0) / CPU_DIVIDER + ACKNOWLEDGE_START;
        // The phase is below E_PERIOD, so it fits.
        let phase = (acknowledge % E_PERIOD) as i32;
        INTERRUPT_CYCLES - ACKNOWLEDGE_NOMINAL + ACKNOWLEDGE_LEAST + phase
    }

    /// Calls the interrupt hook if it is set for `level`, at the entry of `handler`.
    fn run_hook(&mut self, state: &AgState, level: u8, handler: u32) {
        let Some(mut hook) = self.hook.take_if(|hook| hook.level == level) else {
            return;
        };
        let mut stop = InterruptStop {
            level,
            handler,
            taken: self.interrupts[usize::from(level)],
            state,
            devices: &mut self.devices,
        };
        (hook.function)(&mut stop);
        self.hook = Some(hook);
    }

    /// `ag_lookup`: the translated function for `address`, else its stepped function, else
    /// [`no_native_code`].
    pub fn lookup(&mut self, address: u32) -> AgFn {
        if self.too_deep(address) {
            return unwind;
        }
        match self.row(address) {
            Some(function) => function,
            None => self.step_lookup(address),
        }
    }

    /// `ag_step_lookup`: the stepped function for `address`, else [`no_native_code`].
    pub fn step_lookup(&mut self, address: u32) -> AgFn {
        self.stepped(address).unwrap_or_else(|| {
            #[cfg(feature = "interpreter")]
            {
                self.devices.log.miss(address).lookups += 1;
            }
            no_native_code()
        })
    }

    /// The program's checked entry into stepped code (`ag_step_entry`), if it has a stepped
    /// function at `address`.
    fn stepped(&self, address: u32) -> Option<AgFn> {
        if !address.is_multiple_of(2) {
            return None;
        }
        let steps = self.steps.get((address / 2) as usize).copied().flatten();
        steps.and(self.step_entry)
    }

    /// The top level continuing at `address` after a return reached it, or where the last
    /// run ended.
    fn resume(&mut self, address: u32) -> AgFn {
        if let Some(function) = self.row(address).or_else(|| self.stepped(address)) {
            return function;
        }
        #[cfg(feature = "interpreter")]
        if self.between.contains(&address) {
            return interpret_block;
        }
        #[cfg(feature = "interpreter")]
        {
            self.devices.log.miss(address).resumes += 1;
        }
        no_native_code()
    }

    /// Whether a stepped instruction or the interrupt entry is running, whose accesses
    /// leave stalls and rescheduling to its end.
    pub fn stepping(&self) -> bool {
        self.stepping
    }

    /// `ag_step_begin`: notes the start of a stepped instruction, as the interpreter notes
    /// each instruction's.
    pub fn step_begin(&mut self, state: &AgState) {
        let began = self.devices.clock_at(self.clock(state));
        self.step = self.note(state, began);
        self.stepping = true;
    }

    /// What the latch rule needs of an instruction that began at master clock `began`.
    fn note(&self, state: &AgState, began: u64) -> NativeAccess {
        NativeAccess {
            began,
            event: self.devices.hardware.next_interrupt(),
            waiting: self.due(mask(state.sr)).is_some(),
            at: began,
        }
    }

    /// `ag_step_enter`: a check point's slow path, before stepped code runs the rest of its
    /// block. Gives back the `ahead` cycles charged for instructions not yet run, then
    /// applies the latch rule to the translated instruction before (`latch`), if any.
    pub fn step_enter(&mut self, state: &mut AgState, ahead: u32, latch: u32) {
        self.slow_paths += 1;
        // A segment's charge is at most a block's cycles, so it fits.
        state.cycles += ahead as i32;
        if latch != NO_LATCH {
            self.latch_native(state, latch);
        }
    }

    /// The time into the current budget the hardware would see with `cycles` left.
    fn clock_with(&self, cycles: i32) -> u32 {
        let used = i64::from(self.budget) - i64::from(cycles);
        let elapsed = u32::try_from(used.max(0)).unwrap_or(u32::MAX);
        elapsed + self.refresh.pending(elapsed.saturating_sub(self.waited))
    }

    /// Runs one instruction at `cpu.pc`, lifted from the cache when it lies in ROM.
    ///
    /// # Safety
    /// As for [`Context::bus`].
    #[cfg(feature = "interpreter")]
    unsafe fn step(&mut self, state: &mut AgState, cpu: &mut Cpu) -> Step {
        // SAFETY: forwarded from the caller.
        let rom = unsafe { rom(state) };
        let pc = cpu.pc.get();
        let elapsed = self.clock(state);
        let Self {
            devices,
            interpreter,
            lifts,
            ..
        } = self;
        let mut bus = SystemBus {
            ram: &mut state.ram,
            rom,
            devices,
            elapsed,
        };
        match cached(lifts, rom, pc) {
            Some(lifted) => interpreter.run(lifted, cpu, &mut bus),
            None => interpreter.step(cpu, &mut bus),
        }
    }
}

/// The interrupt mask of a status register's system byte.
fn mask(sr: u16) -> u8 {
    (sr >> 8 & 7) as u8
}

/// The state of a running run and its context.
///
/// # Safety
/// `state` must be the state of a running [`Runtime::run`], and no other borrow of it or of
/// its context may be in use while the returned ones are.
unsafe fn parts<'a>(state: *mut AgState) -> (&'a mut AgState, &'a mut Context) {
    // SAFETY: forwarded from the caller; the context lives outside the state.
    unsafe {
        let state = &mut *state;
        let context = &mut *state.runtime.cast::<Context>();
        (state, context)
    }
}

/// `ag_check`: the scheduler at a native check point, where an interrupt it takes is late
/// unless the budget was cut short for it (see the module docs).
///
/// # Safety
/// As for [`check`].
pub(crate) unsafe fn check_native(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { schedule(state, pc, true) }
}

/// The scheduler at an instruction boundary of the interpreter.
///
/// # Safety
/// `state` must be the state of a running [`Runtime::run`], with nothing borrowing it or its
/// context.
unsafe fn check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { schedule(state, pc, false) }
}

/// The scheduler: charges the budget's cycles, applies the depth guard and the run limits,
/// delivers every interrupt the mask lets through (handlers run natively from here), and
/// starts the next budget. Returns `pc` to continue, or where execution goes instead.
///
/// # Safety
/// As for [`check`].
unsafe fn schedule(state: *mut AgState, pc: u32, native: bool) -> u32 {
    // SAFETY: every borrow from `parts` ends before the next native call.
    unsafe {
        // Native code ran its budget out itself: an instruction boundary may have passed.
        let late = native && !parts(state).1.cut;
        loop {
            let (st, ctx) = parts(state);
            ctx.charge(st);
            if ctx.too_deep(pc) {
                return UNWIND | pc;
            }
            if let Some(exit) = ctx.limit(pc) {
                // The next run takes this check up again, as if the run had gone on.
                ctx.recheck = Some(native);
                return ctx.end(exit);
            }
            let Some(level) = ctx.due(mask(st.sr)) else {
                ctx.latched_late = false;
                break;
            };
            if std::mem::take(&mut ctx.latched_late) {
                ctx.one_more = true;
                break;
            }
            if late {
                ctx.late += 1;
            }
            let next = interrupt(state, level, pc);
            if next != pc {
                return next;
            }
        }
        let (st, ctx) = parts(state);
        if ctx.hangs(st, pc) {
            return ctx.end(Exit::Idle { pc });
        }
        ctx.refill(st);
        pc
    }
}

/// `ag_deliver`, also called by the interpreter: after a write to SR, at the instruction
/// boundary `pc`, takes every interrupt the new mask lets through. Never ends the run.
///
/// # Safety
/// As for [`check`].
pub(crate) unsafe fn deliver(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: every borrow from `parts` ends before the next native call.
    unsafe {
        let (st, ctx) = parts(state);
        if ctx.due(mask(st.sr)).is_none() {
            return pc;
        }
        loop {
            let (st, ctx) = parts(state);
            ctx.charge(st);
            let Some(level) = ctx.due(mask(st.sr)) else {
                break;
            };
            let next = interrupt(state, level, pc);
            if next != pc {
                return next;
            }
        }
        let (st, ctx) = parts(state);
        ctx.refill(st);
        pc
    }
}

/// `ag_step_check`: the scheduler at the stepped instruction boundary `pc`, as the
/// interpreter calls it.
///
/// # Safety
/// As for [`check`].
pub(crate) unsafe fn step_check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { check(state, pc) }
}

/// `ag_step_end` and `ag_step_late`: what the interpreter does after an instruction, for the
/// stepped instruction that just ran, going on at `next`; `latch` is its latch point.
/// `charged` holds the cycles it charged when it took the fast path (no `ag_step_begin`: it
/// reached no device and wrote no SR), so its start is worked out from them. By `kind`:
/// delivery at `next` of an interrupt due, then the scheduler there if the budget ran out,
/// as the interpreter does before its next instruction (`AG_STEP_GO`); a check soon (a
/// call, whose target takes it); or nothing (STOP, which waits for one). Returns where
/// execution continues.
///
/// # Safety
/// As for [`check`].
pub(crate) unsafe fn step_end(
    state: *mut AgState,
    next: u32,
    latch: u32,
    kind: u32,
    charged: Option<u32>,
) -> u32 {
    // SAFETY: every borrow from `parts` ends before the next native call.
    unsafe {
        let (st, ctx) = parts(state);
        ctx.stepping = false;
        if let Some(charged) = charged {
            // An instruction charges far below i32::MAX.
            let began = ctx
                .devices
                .clock_at(ctx.clock_with(st.cycles + charged as i32));
            ctx.step = ctx.note(st, began);
        }
        ctx.charge_stall(st);
        let step = ctx.step;
        let latched = step.began + u64::from(latch) * CPU_DIVIDER;
        let ended = ctx.devices.clock_at(ctx.clock(st));
        if !step.waiting
            && step
                .event
                .is_some_and(|event| (latched + 1..=ended).contains(&event))
        {
            ctx.latched_late = true;
        }
        if ctx.devices.take_rescheduled() {
            ctx.reschedule(st);
        }
        // One that came after the latch waits for the next instruction's end.
        let due = !ctx.latched_late && ctx.due(mask(st.sr)).is_some();
        match kind {
            STEP_CALL if due => {
                ctx.check_soon(st);
                return next;
            }
            STEP_GO if due => {
                let after = deliver(state, next);
                if after != next {
                    return after;
                }
            }
            STEP_GO => {}
            // A call's target checks; STOP waits for the interrupt in `ag_stop`.
            _ => return next,
        }
        // As the interpreter does at the next boundary, so the next stepped function needs
        // no check of its own.
        if parts(state).0.cycles <= 0 {
            return check(state, next);
        }
        next
    }
}

/// `ag_stop`, also called by the interpreter and by the top level for a run that ended
/// while stopped: time moves from event to event until an interrupt gets past the mask, which
/// is then taken with `resume` in its frame. Returns where execution continues.
///
/// # Safety
/// As for [`check`].
pub(crate) unsafe fn stop(state: *mut AgState, resume: u32) -> u32 {
    // SAFETY: every borrow from `parts` ends before the next native call.
    unsafe {
        loop {
            let (st, ctx) = parts(state);
            // A run that ends here stays stopped: the top level waits again when it continues.
            ctx.stopped = Some(resume);
            ctx.charge(st);
            if ctx.too_deep(resume) {
                return UNWIND | resume;
            }
            if let Some(exit) = ctx.limit(resume) {
                return ctx.end(exit);
            }
            if let Some(level) = ctx.due(mask(st.sr)) {
                ctx.stopped = None;
                return interrupt(state, level, resume);
            }
            if ctx.waits_forever(mask(st.sr)) {
                return ctx.end(Exit::Stopped { resume });
            }
            // Nothing runs while stopped, so the wait to the next event is used at once, and
            // it causes no refresh; the Z80 takes nothing from a CPU that waits.
            ctx.devices.take_stall();
            let until = ctx.horizon(ctx.devices.clock_at(0), st);
            ctx.budget = until;
            ctx.waited = until.unsigned_abs();
        }
    }
}

/// Takes the interrupt at `level` before `pc` and calls its handler natively; returns the
/// handler's result, `pc` unless it rewrote the stacked PC. The interrupt hook, if one is
/// set for `level`, runs between the two.
///
/// # Safety
/// As for [`check`].
unsafe fn interrupt(state: *mut AgState, level: u8, pc: u32) -> u32 {
    // SAFETY: the borrows end before the entry and the handler run, which derive their own.
    unsafe {
        let (_, ctx) = parts(state);
        ctx.begin_interrupt(level, pc);
        let handler = match ctx.interrupt_entry {
            Some(entry) => {
                ctx.stepping = true;
                let handler = entry(state, level.into(), pc);
                parts(state).1.stepping = false;
                handler
            }
            #[cfg(feature = "interpreter")]
            None => {
                let (st, ctx) = parts(state);
                ctx.interpret_interrupt_entry(st, level, pc)
            }
            #[cfg(not(feature = "interpreter"))]
            None => panic!("a runtime without the interpreter needs the program's ag_interrupt"),
        };
        let (st, ctx) = parts(state);
        ctx.entered_interrupt(st);
        ctx.run_hook(st, level, handler);
        let function = ctx.lookup(handler);
        function(state, handler)
    }
}

/// The entry `ag_lookup` hands out for an address with no native code: the interpreter in a
/// runtime that has it, else `end_without_native_code`.
fn no_native_code() -> AgFn {
    #[cfg(feature = "interpreter")]
    {
        interpret
    }
    #[cfg(not(feature = "interpreter"))]
    {
        end_without_native_code
    }
}

/// Ends the run at `pc`, which has no native code ([`Exit::NoNativeCode`]).
///
/// # Safety
/// Called by translated code or the top level, with the state of a running
/// [`Runtime::run`].
#[cfg(not(feature = "interpreter"))]
unsafe extern "C" fn end_without_native_code(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { parts(state).1.end(Exit::NoNativeCode { pc }) }
}

/// Returns the lift of the instruction at `pc` when it lies wholly in ROM, lifting it on
/// first use. Undecodable words are left to [`Interpreter::step`], which raises the exception.
#[cfg(feature = "interpreter")]
fn cached<'a>(lifts: &'a mut HashMap<u32, Lifted>, rom: &[u8], pc: u32) -> Option<&'a Lifted> {
    use std::collections::hash_map::Entry;

    match lifts.entry(pc) {
        Entry::Occupied(entry) => Some(entry.into_mut()),
        Entry::Vacant(entry) => {
            if pc as usize >= rom.len() {
                return None;
            }
            let instruction = anygenesis_m68k::decode_at(rom, pc as usize).ok()?;
            // The loader refuses images over 4 MiB, so the size fits.
            if Region::of(pc, instruction.length(), rom.len() as u32) != Region::Rom {
                return None;
            }
            let lifted = lift(&instruction, Address::new(pc)).ok()?;
            Some(entry.insert(lifted))
        }
    }
}

/// The ROM a state points at.
///
/// # Safety
/// `state.rom` must point at `state.rom_size` bytes that stay readable for `'a`.
unsafe fn rom<'a>(state: &AgState) -> &'a [u8] {
    // SAFETY: forwarded from the caller.
    unsafe { slice::from_raw_parts(state.rom, state.rom_size as usize) }
}

/// The ROM image and work RAM of a run's state, for the Z80.
fn memory(state: &mut AgState) -> (&[u8], &mut [u8; RAM_SIZE]) {
    // SAFETY: a context only ever runs with the state its Runtime set up, whose `rom` points
    // at the Runtime's ROM image for as long as both live.
    let rom = unsafe { rom(state) };
    (rom, &mut state.ram)
}

/// Where the native stack is now, to measure depth against the top-level loop's position.
#[inline(never)]
fn stack_position() -> usize {
    let marker = 0u8;
    std::hint::black_box(&raw const marker).addr()
}

/// The `AgFn` the dispatcher hands out when the native stack must unwind first.
extern "C" fn unwind(_state: *mut AgState, pc: u32) -> u32 {
    UNWIND | pc
}

#[cfg(feature = "interpreter")]
fn load_cpu(state: &AgState, pc: u32) -> Cpu {
    let flags = [
        (state.x, Flag::X),
        (state.n, Flag::N),
        (state.z, Flag::Z),
        (state.v, Flag::V),
        (state.c, Flag::C),
    ];
    let sr = flags
        .into_iter()
        .filter(|&(value, _)| value != 0)
        .fold(state.sr, |sr, (_, flag)| sr | 1 << flag.bit());
    // `new` places the stack pointers; the state's are copied over it as they are.
    let mut cpu = Cpu::new(sr, 0, 0);
    cpu.d = state.d;
    cpu.a = state.a;
    cpu.inactive_sp = state.inactive_sp;
    cpu.pc = Address::new(pc);
    cpu
}

#[cfg(feature = "interpreter")]
fn store_cpu(cpu: &Cpu, state: &mut AgState) {
    state.d = cpu.d;
    state.a = cpu.a;
    state.inactive_sp = cpu.inactive_sp;
    state.pc = cpu.pc.get();
    state.sr = cpu.sr() & 0xFF00;
    state.x = cpu.flag(Flag::X).into();
    state.n = cpu.flag(Flag::N).into();
    state.z = cpu.flag(Flag::Z).into();
    state.v = cpu.flag(Flag::V).into();
    state.c = cpu.flag(Flag::C).into();
}

/// The interpreter entry `ag_lookup` hands out for addresses without translated code.
///
/// It runs instructions until the next one has an `ag_table` row, then returns that address
/// (see the module docs). A JSR or BSR becomes a native call compared against its return
/// address, as in translated code; RTS, RTR and RTE return the popped address. Every
/// instruction boundary is a check point once the budget runs out, and an interrupt made due
/// by an instruction (a device access, a lowered mask) is taken before the next one.
///
/// # Safety
/// Called by translated code or the top level, with the state of a running
/// [`Runtime::run`].
#[cfg(feature = "interpreter")]
unsafe extern "C" fn interpret(state: *mut AgState, start: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    match unsafe { run_interpreter(state, start, true) } {
        Ending::Row(next) | Ending::Leave(next) => next,
    }
}

/// The entry the top level goes on through where the last run ended between table rows
/// with no stepped code there: the rest of that block, run up to the next table row
/// without counting a miss.
///
/// # Safety
/// As for [`interpret`].
#[cfg(feature = "interpreter")]
unsafe extern "C" fn interpret_block(state: *mut AgState, start: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    match unsafe { run_interpreter(state, start, false) } {
        Ending::Row(next) | Ending::Leave(next) => next,
    }
}

/// How the interpreter handed control back.
#[cfg(feature = "interpreter")]
enum Ending {
    /// The next instruction has a table row.
    Row(u32),
    /// A return, a call that came back elsewhere, a rewritten stacked PC or an unwind.
    Leave(u32),
}

/// Runs instructions from `start` until the next one has an `ag_table` row (see
/// [`interpret`]). `miss` counts them as a dispatcher miss; resuming between rows counts
/// none.
///
/// # Safety
/// As for [`interpret`].
#[cfg(feature = "interpreter")]
unsafe fn run_interpreter(state: *mut AgState, start: u32, miss: bool) -> Ending {
    // SAFETY: the state of a running run points at its context, and neither is borrowed
    // while this function runs; borrows made here end before every nested native call,
    // which derives its own.
    unsafe {
        let mut cpu = load_cpu(&*state, start);
        let mut first = true;
        loop {
            let pc = cpu.pc.get();
            let (st, _) = parts(state);
            // A budget that ran out here is this boundary's, even where a row starts.
            if st.cycles <= 0 {
                store_cpu(&cpu, st);
                let next = check(state, pc);
                if next != pc {
                    return Ending::Leave(next);
                }
                cpu = load_cpu(&*state, pc);
            }
            let (st, ctx) = parts(state);
            if !first && ctx.row(pc).is_some() {
                store_cpu(&cpu, st);
                return Ending::Row(pc);
            }
            first = false;
            // One already due is latched by any instruction; the next one by this instruction
            // only if it comes before the latch point.
            let began = ctx.devices.clock_at(ctx.clock(st));
            let event = ctx.devices.hardware.next_interrupt();
            let waiting = ctx.due(mask(cpu.sr())).is_some();
            let step = ctx.step(st, &mut cpu);
            st.cycles -= i32::from(step.cycles);
            ctx.charge_stall(st);
            let latched = began + u64::from(step.latch) * CPU_DIVIDER;
            let ended = ctx.devices.clock_at(ctx.clock(st));
            if !waiting && event.is_some_and(|event| (latched + 1..=ended).contains(&event)) {
                ctx.latched_late = true;
            }
            if miss {
                ctx.devices.log.miss(start).instructions += 1;
            }
            if step.outcome.reset_devices {
                ctx.devices.reset();
            }
            if ctx.devices.take_rescheduled() {
                ctx.reschedule(st);
            }
            // One that came after the latch waits for the next instruction's end.
            let due = !ctx.latched_late && ctx.due(mask(cpu.sr())).is_some();
            match step.outcome.transfer {
                Some(Transfer::Call {
                    target,
                    return_address,
                }) => {
                    if due {
                        ctx.check_soon(st);
                    }
                    store_cpu(&cpu, st);
                    let function = ctx.lookup(target.get());
                    let next = function(state, target.get());
                    if next != return_address.get() {
                        return Ending::Leave(next);
                    }
                    cpu = load_cpu(&*state, next);
                }
                // As translated code does: an interrupt RTE let through is taken at its target.
                Some(Transfer::Return { target, .. }) => {
                    store_cpu(&cpu, st);
                    let next = if due {
                        deliver(state, target.get())
                    } else {
                        target.get()
                    };
                    return Ending::Leave(next);
                }
                Some(Transfer::Stop { resume }) => {
                    store_cpu(&cpu, st);
                    let next = stop(state, resume.get());
                    if next != resume.get() {
                        return Ending::Leave(next);
                    }
                    cpu = load_cpu(&*state, next);
                }
                // An interrupt the instruction made due is taken before the next one.
                _ if due => {
                    let next = cpu.pc.get();
                    store_cpu(&cpu, st);
                    let after = deliver(state, next);
                    if after != next {
                        return Ending::Leave(after);
                    }
                    cpu = load_cpu(&*state, next);
                }
                _ => {}
            }
        }
    }
}

/// Recompiled code, its ROM and the machine it runs on.
#[derive(Debug)]
pub struct Runtime {
    // The state points into `rom`, whose bytes stay where they are while it is owned here.
    rom: Rom,
    state: Box<AgState>,
    context: Box<Context>,
}

impl Runtime {
    /// Returns the machine at reset: the ROM attached, SSP and PC from its vectors,
    /// supervisor mode with interrupts masked, registers and RAM zero. `table` is the
    /// translated program's `ag_table`, sorted by address.
    pub fn new(
        rom: Rom,
        table: &'static [AgEntry],
        hardware: Box<dyn Hardware>,
        options: Options,
    ) -> Self {
        // SAFETY: every field of AgState is an integer, an integer array or a raw pointer, for
        // which all-zero bytes are a valid value.
        let mut state = unsafe { Box::<AgState>::new_zeroed().assume_init() };
        let bytes = rom.bytes();
        state.rom = bytes.as_ptr();
        // The loader refuses images over 4 MiB, so the size fits.
        state.rom_size = bytes.len() as u32;
        state.a[7] = rom.vectors().initial_ssp();
        state.pc = rom.vectors().reset_pc() & ADDRESS_MASK;
        state.sr = RESET_SR;
        let mut context = Box::new(Context {
            devices: Devices::new(hardware, options.version, options.open_bus),
            table,
            steps: &[],
            step_entry: None,
            interrupt_entry: None,
            budget: 0,
            waited: 0,
            refresh: Refresh::default(),
            options,
            #[cfg(feature = "interpreter")]
            interpreter: Interpreter::default(),
            #[cfg(feature = "interpreter")]
            lifts: HashMap::new(),
            stack_base: 0,
            exit: None,
            cycles: 0,
            stopped: None,
            frame_limit: None,
            interrupts: [0; 8],
            late: 0,
            cut: false,
            slow_paths: 0,
            stepping: false,
            step: NativeAccess::default(),
            latched_late: false,
            one_more: false,
            access: NativeAccess::default(),
            recheck: None,
            #[cfg(feature = "interpreter")]
            between: HashSet::new(),
            idle: None,
            hook: None,
        });
        context.refill(&mut state);
        Self {
            rom,
            state,
            context,
        }
    }

    /// Attaches the program's stepped code: `steps` holds the stepped function of each even
    /// ROM address (index address / 2, `None` where it has none), `entry` its
    /// `ag_step_entry`, which the dispatcher hands out for them, and `interrupt` its
    /// `ag_interrupt`. A runtime without the interpreter needs them.
    pub fn with_stepped(
        mut self,
        steps: &'static [Option<AgFn>],
        entry: AgFn,
        interrupt: AgInterrupt,
    ) -> Self {
        self.context.steps = steps;
        self.context.step_entry = Some(entry);
        self.context.interrupt_entry = Some(interrupt);
        self
    }

    /// Runs from `state().pc` (or the STOP the last run ended in) until the run ends.
    pub fn run(&mut self) -> Exit {
        self.context.frame_limit = None;
        self.go()
    }

    /// Runs until `frames` more frames have ended, or an earlier limit.
    pub fn run_frames(&mut self, frames: u64) -> Exit {
        let limit = self.context.devices.hardware.frames() + frames;
        self.context.frame_limit = Some(limit);
        let exit = self.go();
        self.context.frame_limit = None;
        exit
    }

    fn go(&mut self) -> Exit {
        let state: *mut AgState = &raw mut *self.state;
        let context: *mut Context = &raw mut *self.context;
        // SAFETY: `state` and `context` stay valid for the whole run, nothing else holds a
        // reference to them, and each borrow below ends before translated code runs. The
        // externs reach the context through `state.runtime` only while this loop runs.
        unsafe {
            (*context).stack_base = stack_position();
            (*context).exit = None;
            (*state).runtime = context.cast();
            let mut pc = (*state).pc;
            let exit = loop {
                let recheck = (*context).recheck.take();
                let next = match ((*context).stopped, recheck) {
                    (Some(resume), _) => stop(state, resume),
                    (None, Some(native)) => schedule(state, pc, native),
                    (None, None) => {
                        let function = (*context).resume(pc);
                        function(state, pc)
                    }
                };
                pc = next & ADDRESS_MASK;
                if next & UNWIND != 0
                    && let Some(exit) = (*context).exit.take()
                {
                    break exit;
                }
            };
            (*state).pc = pc;
            (*state).runtime = ptr::null_mut();
            exit
        }
    }

    pub fn rom(&self) -> &Rom {
        &self.rom
    }

    pub fn state(&self) -> &AgState {
        &self.state
    }

    /// The state, to set up registers or RAM before a run.
    pub fn state_mut(&mut self) -> &mut AgState {
        &mut self.state
    }

    pub fn log(&self) -> &Log {
        &self.context.devices.log
    }

    /// Cycles charged so far, counted at each check point.
    pub fn cycles(&self) -> u64 {
        self.context.cycles
    }

    /// Frames that have ended so far, as the hardware counts them.
    pub fn frames(&self) -> u64 {
        self.context.devices.hardware.frames()
    }

    /// Interrupts taken at `level` (1-7) so far.
    pub fn interrupts(&self, level: u8) -> u64 {
        self.context
            .interrupts
            .get(usize::from(level))
            .copied()
            .unwrap_or(0)
    }

    /// Interrupts taken at a native check point that found the budget spent by the cycles
    /// run, so the instruction boundary they were due at may have passed (see the module
    /// docs). A translated program with stepped code shows none.
    pub fn late_interrupts(&self) -> u64 {
        self.context.late
    }

    /// Check points whose slow path ran the rest of their block as stepped code, because the
    /// budget could end inside it.
    pub fn slow_paths(&self) -> u64 {
        self.context.slow_paths
    }

    /// Whether the CPU is waiting in a STOP.
    pub fn stopped(&self) -> bool {
        self.context.stopped.is_some()
    }

    /// The hardware, if it is a `T`.
    pub fn hardware<T: Hardware>(&self) -> Option<&T> {
        (self.context.devices.hardware.as_ref() as &dyn Any).downcast_ref()
    }

    /// Z80 RAM, as far as the Z80 has run.
    pub fn z80_ram(&self) -> &[u8; z80::RAM_SIZE] {
        self.context.devices.z80.ram()
    }

    /// The Z80 side: its registers, bank register and lines. The Z80 runs ahead of the
    /// 68000, up to where the last budget would have ended.
    pub fn z80(&self) -> &z80::Z80 {
        &self.context.devices.z80
    }

    /// Writes to the sound chips so far, by chip and CPU; the Z80's include those it ran
    /// ahead of the 68000.
    pub fn sound_counts(&self) -> SoundCounts {
        let devices = &self.context.devices;
        devices.sound.counts().sum(&devices.z80.sound().counts())
    }

    /// Keeps every sound write from now on, for [`Runtime::take_sound_writes`].
    pub fn record_sound(&mut self, recording: bool) {
        let devices = &mut self.context.devices;
        devices.sound.set_recording(recording);
        devices.z80.sound_mut().set_recording(recording);
    }

    /// The master clock the 68000 has reached; the Z80 may have run past it.
    pub fn clock(&self) -> u64 {
        self.context.devices.clock_at(0)
    }

    /// Takes the sound writes kept so far that the 68000 has reached ([`Runtime::clock`]),
    /// in time order; none of them can be rolled back.
    pub fn take_sound_writes(&mut self) -> Vec<SoundWrite> {
        let devices = &mut self.context.devices;
        let now = devices.clock_at(0);
        let mut writes = devices.sound.take_before(now);
        writes.extend(devices.z80.sound_mut().take_before(now));
        writes.sort_by_key(|write| write.clock);
        writes
    }

    /// Whether TMSS lets the VDP run (always, on a console without TMSS).
    pub fn tmss_unlocked(&self) -> bool {
        self.context.devices.tmss_unlocked()
    }

    /// Sets the buttons held on the pad in port 1.
    pub fn set_pad(&mut self, buttons: Buttons) {
        self.context.devices.io.set_pad(buttons);
    }

    /// Calls `hook` at every entry of the level `level` handler from now on, replacing any
    /// earlier hook: after the exception frame is pushed, before the handler's first
    /// instruction, which is where a debugger breakpoint on the handler stops.
    pub fn set_interrupt_hook(
        &mut self,
        level: u8,
        hook: impl FnMut(&mut InterruptStop<'_>) + 'static,
    ) {
        self.context.hook = Some(InterruptHook {
            level,
            function: Box::new(hook),
        });
    }
}
