//! Running translated code: the top-level loop, the dispatcher with its interpreter fallback,
//! the run limits and the native-depth guard.
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
//! - [`UNWIND`] marks a return value no return check can match (they compare 24-bit
//!   addresses): it carries a pc in its low bits down to the top level, which resumes there
//!   with an empty native stack or ends the run.

use std::{any::Any, collections::HashMap, fmt, ptr, slice};

use anygenesis_abi::{AgEntry, AgFn, AgState, RAM_SIZE, Region};
use anygenesis_ir::{
    Flag, Lifted,
    interp::{Cpu, Interpreter, Step, Transfer},
    lift,
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

/// Settings for a run.
#[derive(Debug, Clone)]
pub struct Options {
    /// The version register: region, video standard, expansion unit, hardware version.
    pub version: u8,
    /// What reads of unmapped addresses return.
    pub open_bus: u16,
    /// Cycles between calls of the check-point hook.
    pub slice: i32,
    /// The run ends once this many cycles have been charged.
    pub max_cycles: u64,
    /// End the run at the first idle loop.
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
            // One NTSC scanline.
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
    /// A check point came round again at `pc` with registers and RAM unchanged: a wait that
    /// only an interrupt or a device can end.
    Idle { pc: u32 },
    /// STOP, continuing at `resume`; nothing can raise an interrupt yet.
    Stopped { resume: u32 },
    /// `Options::max_cycles` ran out at `pc`.
    CycleCap { pc: u32 },
}

impl Exit {
    /// The address execution continues at.
    pub fn pc(self) -> u32 {
        match self {
            Self::Idle { pc } | Self::Stopped { resume: pc } | Self::CycleCap { pc } => pc,
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle { pc } => write!(f, "idle loop at ${pc:06X}"),
            Self::Stopped { resume } => write!(f, "STOP, resuming at ${resume:06X}"),
            Self::CycleCap { pc } => write!(f, "cycle cap at ${pc:06X}"),
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
    fn idle(previous: &mut Option<Self>, pc: u32, state: &AgState) -> bool {
        let (registers, flags) = Self::registers(state);
        match previous {
            Some(snapshot)
                if snapshot.pc == pc
                    && snapshot.registers == registers
                    && snapshot.flags == flags =>
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
    cycles: u64,
    idle: Option<Snapshot>,
}

impl Context {
    fn row(&self, address: u32) -> Option<AgFn> {
        self.table
            .binary_search_by_key(&address, |entry| entry.address)
            .ok()
            .map(|index| self.table[index].function)
    }

    /// The bus for one access or instruction.
    ///
    /// # Safety
    /// `state.rom` must point at `state.rom_size` readable bytes, as [`Runtime`] sets them.
    pub unsafe fn bus<'a>(&'a mut self, state: &'a mut AgState) -> SystemBus<'a> {
        SystemBus {
            // SAFETY: forwarded from the caller.
            rom: unsafe { rom(state) },
            ram: &mut state.ram,
            devices: &mut self.devices,
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

    /// After a bus extern: a read or write cannot unwind, so a deep stack makes the next
    /// check point call the hook, which can.
    pub fn after_access(&mut self, state: &mut AgState, address: u32) {
        if self.depth(address) > self.options.soft_depth {
            state.cycles = state.cycles.min(0);
        }
    }

    /// Adds the cycles used since the last refill to the run's total and refills.
    fn charge(&mut self, state: &mut AgState) {
        let used = i64::from(self.options.slice) - i64::from(state.cycles);
        let used = u32::try_from(used.max(0)).unwrap_or(u32::MAX);
        self.cycles += u64::from(used);
        self.devices.hardware.advance(used);
        state.cycles = self.options.slice;
    }

    /// Ends the run at `pc`.
    fn end(&mut self, exit: Exit) -> u32 {
        self.exit = Some(exit);
        UNWIND | exit.pc()
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

    /// `ag_check`: no interrupts exist yet, so the hook only keeps time, guards the native
    /// stack and applies the run limits.
    pub fn check(&mut self, state: &mut AgState, pc: u32) -> u32 {
        self.charge(state);
        if self.too_deep(pc) {
            return UNWIND | pc;
        }
        if self.cycles >= self.options.max_cycles {
            return self.end(Exit::CycleCap { pc });
        }
        if self.options.stop_when_idle && Snapshot::idle(&mut self.idle, pc, state) {
            return self.end(Exit::Idle { pc });
        }
        pc
    }

    /// `ag_stop`: with nothing to raise an interrupt the wait would never end, so the run
    /// ends instead.
    pub fn stop(&mut self, state: &mut AgState, resume: u32) -> u32 {
        self.charge(state);
        self.end(Exit::Stopped { resume })
    }

    /// Runs one instruction at `cpu.pc`, lifted from the cache when it lies in ROM.
    ///
    /// # Safety
    /// As for [`Context::bus`].
    unsafe fn step(&mut self, state: &mut AgState, cpu: &mut Cpu) -> Step {
        // SAFETY: forwarded from the caller.
        let rom = unsafe { rom(state) };
        let pc = cpu.pc.get();
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
        };
        match cached(lifts, rom, pc) {
            Some(lifted) => interpreter.run(lifted, cpu, &mut bus),
            None => interpreter.step(cpu, &mut bus),
        }
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
/// address, as in translated code; RTS, RTR and RTE return the popped address.
///
/// # Safety
/// Called by translated code or the top level, with the state of a running
/// [`Runtime::run`].
unsafe extern "C" fn interpret(state: *mut AgState, start: u32) -> u32 {
    // SAFETY: the state of a running run points at its context, and neither is borrowed
    // while this function runs; borrows made here end before every nested native call,
    // which derives its own.
    unsafe {
        let context = (*state).runtime.cast::<Context>();
        let mut cpu = load_cpu(&*state, start);
        let mut first = true;
        loop {
            let pc = cpu.pc.get();
            let ctx = &mut *context;
            let st = &mut *state;
            if !first && ctx.row(pc).is_some() {
                store_cpu(&cpu, st);
                return pc;
            }
            first = false;
            if st.cycles <= 0 {
                store_cpu(&cpu, st);
                let next = ctx.check(st, pc);
                if next != pc {
                    return next;
                }
                cpu = load_cpu(st, pc);
            }
            let step = ctx.step(st, &mut cpu);
            st.cycles -= i32::from(step.cycles);
            ctx.devices.log.miss(start).instructions += 1;
            if step.outcome.reset_devices {
                ctx.devices.reset();
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
                    return ctx.stop(st, resume.get());
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
            options,
            interpreter: Interpreter::default(),
            lifts: HashMap::new(),
            stack_base: 0,
            exit: None,
            cycles: 0,
            idle: None,
        });
        Self {
            rom,
            state,
            context,
        }
    }

    /// Runs from `state().pc` until the run ends.
    pub fn run(&mut self) -> Exit {
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
                let function = (*context).resume(pc);
                let next = function(state, pc);
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
