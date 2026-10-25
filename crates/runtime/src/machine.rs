//! Running translated code: the top-level loop, the dispatcher with its interpreter fallback,
//! the scheduler that keeps time and delivers interrupts, the run limits and the
//! native-depth guard.
//!
//! Every translated function returns the address execution continues at, and every caller
//! compares it with the return address it expects. The runtime uses that one convention for
//! all of its own control:
//!
//! - A return no native frame expects cascades to [`Runtime::run`], which continues at the
//!   returned address through `ag_table`.
//! - The interpreter entry hands control back by returning the first table address it
//!   reaches, so a caller expecting that address continues natively and the top level
//!   resumes anything else.
//! - An interrupt handler is called natively from a check point with its frame pushed, and
//!   returns the stacked PC through RTE. The check point continues when that is its own
//!   address and returns anything else, so a handler that rewrote the stacked PC goes
//!   through the dispatcher.
//! - [`UNWIND`] marks a return value no return check can match (they compare 24-bit
//!   addresses): it carries a pc in its low bits down to the top level, which resumes there
//!   with an empty native stack or ends the run.
//!
//! Time: translated code counts `AgState::cycles` down from a budget the scheduler sets,
//! and calls `ag_check` at the next check point once it runs out. The budget ends at the
//! hardware's next event, so interrupts arrive at the first check point after they are
//! raised; while one waits behind the interrupt mask the budget is zero, so a lowered mask
//! is seen at the next check point.

use std::{any::Any, collections::HashMap, fmt, ptr, slice};

use anygenesis_abi::{AgEntry, AgFn, AgState, RAM_SIZE, Region};
use anygenesis_ir::{
    Flag, Lifted,
    interp::{Cpu, Interpreter, Step, Transfer},
    interrupt_entry, lift,
};
use anygenesis_rom::Rom;
use anygenesis_shared::Address;

use crate::{
    bus::{Devices, SystemBus},
    hardware::Hardware,
    io::{Buttons, VERSION_NO_EXPANSION, VERSION_OVERSEAS},
    log::Log,
    z80,
};

/// Set in a return value that must reach the top level; the low 24 bits hold a pc.
pub const UNWIND: u32 = 0x8000_0000;

/// A thread stack with room for the native-depth guard's default limits.
pub const RUN_STACK: usize = 64 << 20;

const ADDRESS_MASK: u32 = 0xFF_FFFF;

/// The status register after reset: supervisor mode, every interrupt masked.
const RESET_SR: u16 = 0x2700;

/// Cycles the 68000 spends taking an interrupt (MC68000 User's Manual, Table 8-14).
const INTERRUPT_CYCLES: i32 = 44;

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
            // One NTSC scanline, rounded down.
            slice: 488,
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
}

impl Exit {
    /// The address execution continues at.
    pub fn pc(self) -> u32 {
        match self {
            Self::Idle { pc }
            | Self::Stopped { resume: pc }
            | Self::CycleCap { pc }
            | Self::Frames { pc } => pc,
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
    options: Options,
    interpreter: Interpreter,
    /// Lifted instructions in ROM by address; code in RAM is decoded every time it runs.
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
    /// The address after a STOP the CPU is waiting in.
    stopped: Option<u32>,
    /// The frame count at which [`Runtime::run_frames`] ends the run.
    frame_limit: Option<u64>,
    /// Interrupts taken, by level.
    interrupts: [u64; 8],
    idle: Option<Snapshot>,
}

impl Context {
    fn row(&self, address: u32) -> Option<AgFn> {
        self.table
            .binary_search_by_key(&address, |entry| entry.address)
            .ok()
            .map(|index| self.table[index].function)
    }

    /// Cycles used of the current budget.
    fn elapsed(&self, state: &AgState) -> u32 {
        let used = i64::from(self.budget) - i64::from(state.cycles);
        u32::try_from(used.max(0)).unwrap_or(u32::MAX)
    }

    /// The bus for one access or instruction.
    ///
    /// # Safety
    /// `state.rom` must point at `state.rom_size` readable bytes, as [`Runtime`] sets them.
    pub unsafe fn bus<'a>(&'a mut self, state: &'a mut AgState) -> SystemBus<'a> {
        let elapsed = self.elapsed(state);
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
    }

    /// After a bus extern: a read or write cannot unwind or take an interrupt, so a deep
    /// stack or an interrupt the access made due makes the next check point call the hook,
    /// which can.
    pub fn after_access(&mut self, state: &mut AgState, address: u32) {
        self.charge_stall(state);
        if self.depth(address) > self.options.soft_depth || self.due(mask(state.sr)).is_some() {
            self.check_soon(state);
        }
    }

    /// Takes the cycles a device froze the 68000 for out of the budget, as if the access
    /// that caused them had taken that long.
    fn charge_stall(&mut self, state: &mut AgState) {
        state.cycles = state.cycles.saturating_sub(self.devices.take_stall());
    }

    /// Ends the current budget: the cycles used go to the run's total and to the hardware.
    fn charge(&mut self, state: &mut AgState) {
        self.charge_stall(state);
        let used = self.elapsed(state);
        self.cycles += u64::from(used);
        self.devices.settle(used);
        self.budget = 0;
        state.cycles = 0;
    }

    /// Starts a budget that lasts until the hardware's next event, a slice at most; none
    /// while an interrupt waits behind the mask.
    fn refill(&mut self, state: &mut AgState) {
        let hardware = &self.devices.hardware;
        self.budget = if hardware.pending_interrupt().is_some() {
            0
        } else {
            let until = i32::try_from(hardware.until_event()).unwrap_or(i32::MAX);
            self.options.slice.min(until)
        };
        state.cycles = self.budget;
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

    /// Ends the run at `pc`.
    fn end(&mut self, exit: Exit) -> u32 {
        self.exit = Some(exit);
        UNWIND | exit.pc()
    }

    /// Takes the interrupt at `level` before the instruction at `pc`: acknowledges it,
    /// pushes its frame, starts a budget with the exception's own cycles already used, and
    /// returns the handler's address.
    ///
    /// # Safety
    /// As for [`Context::bus`].
    unsafe fn enter_interrupt(&mut self, state: &mut AgState, level: u8, pc: u32) -> u32 {
        self.devices.hardware.acknowledge(level);
        self.interrupts[usize::from(level)] += 1;
        let mut cpu = load_cpu(state, pc);
        let entry = interrupt_entry(level, Address::new(pc));
        // SAFETY: forwarded from the caller.
        let rom = unsafe { rom(state) };
        let elapsed = self.elapsed(state);
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
        self.refill(state);
        state.cycles -= INTERRUPT_CYCLES;
        match outcome.transfer {
            Some(Transfer::Jump(handler)) => handler.get(),
            // The entry sequence always ends in a jump through the vector table.
            _ => unreachable!("an interrupt entry leaves through its vector"),
        }
    }

    /// `ag_lookup`: the translated function for `address`, else the interpreter.
    pub fn lookup(&mut self, address: u32) -> AgFn {
        if self.too_deep(address) {
            return unwind;
        }
        self.row(address).unwrap_or_else(|| {
            self.devices.log.miss(address).lookups += 1;
            interpret
        })
    }

    /// The top level continuing at `address` after a return reached it.
    fn resume(&mut self, address: u32) -> AgFn {
        self.row(address).unwrap_or_else(|| {
            self.devices.log.miss(address).resumes += 1;
            interpret
        })
    }

    /// Runs one instruction at `cpu.pc`, lifted from the cache when it lies in ROM.
    ///
    /// # Safety
    /// As for [`Context::bus`].
    unsafe fn step(&mut self, state: &mut AgState, cpu: &mut Cpu) -> Step {
        // SAFETY: forwarded from the caller.
        let rom = unsafe { rom(state) };
        let pc = cpu.pc.get();
        let elapsed = self.elapsed(state);
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

/// `ag_check`, also called by the interpreter: the scheduler. It charges the budget's
/// cycles, applies the depth guard and the run limits, delivers every interrupt the mask
/// lets through (handlers run natively from here), and starts the next budget. Returns
/// `pc` to continue, or where execution goes instead.
///
/// # Safety
/// `state` must be the state of a running [`Runtime::run`], with nothing borrowing it or its
/// context.
pub(crate) unsafe fn check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: every borrow from `parts` ends before the next native call.
    unsafe {
        loop {
            let (st, ctx) = parts(state);
            ctx.charge(st);
            if ctx.too_deep(pc) {
                return UNWIND | pc;
            }
            if let Some(exit) = ctx.limit(pc) {
                return ctx.end(exit);
            }
            let Some(level) = ctx.due(mask(st.sr)) else {
                break;
            };
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
            // Nothing runs while stopped, so the wait to the next event is used at once.
            ctx.budget = i32::try_from(ctx.devices.hardware.until_event()).unwrap_or(i32::MAX);
        }
    }
}

/// Takes the interrupt at `level` before `pc` and calls its handler natively; returns the
/// handler's result, `pc` unless it rewrote the stacked PC.
///
/// # Safety
/// As for [`check`].
unsafe fn interrupt(state: *mut AgState, level: u8, pc: u32) -> u32 {
    // SAFETY: the borrows end before the handler runs, which derives its own.
    unsafe {
        let (st, ctx) = parts(state);
        let handler = ctx.enter_interrupt(st, level, pc);
        let function = ctx.lookup(handler);
        function(state, handler)
    }
}

/// Returns the lift of the instruction at `pc` when it lies wholly in ROM, lifting it on
/// first use. Undecodable words are left to [`Interpreter::step`], which raises the exception.
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
unsafe extern "C" fn interpret(state: *mut AgState, start: u32) -> u32 {
    // SAFETY: the state of a running run points at its context, and neither is borrowed
    // while this function runs; borrows made here end before every nested native call,
    // which derives its own.
    unsafe {
        let mut cpu = load_cpu(&*state, start);
        let mut first = true;
        loop {
            let pc = cpu.pc.get();
            let (st, ctx) = parts(state);
            if !first && ctx.row(pc).is_some() {
                store_cpu(&cpu, st);
                return pc;
            }
            first = false;
            if st.cycles <= 0 {
                store_cpu(&cpu, st);
                let next = check(state, pc);
                if next != pc {
                    return next;
                }
                cpu = load_cpu(&*state, pc);
            }
            let (st, ctx) = parts(state);
            let step = ctx.step(st, &mut cpu);
            st.cycles -= i32::from(step.cycles);
            ctx.charge_stall(st);
            ctx.devices.log.miss(start).instructions += 1;
            if step.outcome.reset_devices {
                ctx.devices.reset();
            }
            if ctx.due(mask(cpu.sr())).is_some() {
                ctx.check_soon(st);
            }
            match step.outcome.transfer {
                Some(Transfer::Call {
                    target,
                    return_address,
                }) => {
                    store_cpu(&cpu, st);
                    let function = ctx.lookup(target.get());
                    let next = function(state, target.get());
                    if next != return_address.get() {
                        return next;
                    }
                    cpu = load_cpu(&*state, next);
                }
                Some(Transfer::Return { target, .. }) => {
                    store_cpu(&cpu, st);
                    return target.get();
                }
                Some(Transfer::Stop { resume }) => {
                    store_cpu(&cpu, st);
                    let next = stop(state, resume.get());
                    if next != resume.get() {
                        return next;
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
        state.cycles = options.slice;
        let context = Box::new(Context {
            devices: Devices::new(hardware, options.version, options.open_bus),
            table,
            budget: options.slice,
            options,
            interpreter: Interpreter::default(),
            lifts: HashMap::new(),
            stack_base: 0,
            exit: None,
            cycles: 0,
            stopped: None,
            frame_limit: None,
            interrupts: [0; 8],
            idle: None,
        });
        Self {
            rom,
            state,
            context,
        }
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
                let next = match (*context).stopped {
                    Some(resume) => stop(state, resume),
                    None => {
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

    /// Whether the CPU is waiting in a STOP.
    pub fn stopped(&self) -> bool {
        self.context.stopped.is_some()
    }

    /// The hardware, if it is a `T`.
    pub fn hardware<T: Hardware>(&self) -> Option<&T> {
        (self.context.devices.hardware.as_ref() as &dyn Any).downcast_ref()
    }

    /// Z80 RAM as the 68000 last wrote it.
    pub fn z80_ram(&self) -> &[u8; z80::RAM_SIZE] {
        self.context.devices.z80.ram()
    }

    /// Whether TMSS lets the VDP run (always, on a console without TMSS).
    pub fn tmss_unlocked(&self) -> bool {
        self.context.devices.tmss_unlocked()
    }

    /// Sets the buttons held on the pad in port 1.
    pub fn set_pad(&mut self, buttons: Buttons) {
        self.context.devices.io.set_pad(buttons);
    }
}
