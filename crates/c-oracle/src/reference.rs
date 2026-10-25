//! The reference side of a function run: the IR interpreter on the unpruned bodies, with
//! Musashi in lockstep when asked, deciding where the translated code has to stop.
//!
//! Translated code keeps a native frame per call (and exception) whose continuation lies in
//! the calling function; a return checks the innermost such frame first and unwinds to the
//! first one that expects the address. The reference models exactly that with its frame
//! stack, so both sides stop at the same instruction:
//! - every dispatcher call from translated code (an indirect transfer to a target the
//!   analysis does not list, or a target without translated code) ends the run;
//! - a return no native frame expects reaches the top level. It continues there, through
//!   `ag_table`, when it is a real return to some frame and the table has the address
//!   (an arrival); otherwise the stack was overwritten by the random state and the run ends.
//!
//! Flags are compared where the run ends, as far as flag liveness promises them (see
//! `Reference::live_at`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anygenesis_abi::RAM_SIZE;
use anygenesis_analysis::{Analysis, FlagLiveness, Origin, Terminator};
use anygenesis_ir::{
    Control, FlagSet, Lifted, Op,
    interp::{Cpu, Interpreter, Transfer},
};
use anygenesis_shared::Address;

use crate::{
    bus::{Io, SystemBus, Watch},
    lockstep::Lockstep,
    random::SplitMix,
};

/// Where a run returns to; outside the ROM and RAM, so no analysed code lives there.
pub const SENTINEL: u32 = 0x00F0_0000;
/// Sonic 1's initial supervisor stack pointer.
const STACK: u32 = 0x00FF_FE00;
/// Instructions kept for failure messages, and how many of them a message lists.
const TRAIL: usize = 4096;
const TRAIL_SHOWN: usize = 12;

/// Registers, RAM and I/O of one side of a run.
#[derive(Debug, Clone)]
pub struct Machine {
    pub cpu: Cpu,
    pub ram: Box<[u8; RAM_SIZE]>,
    pub io: Io,
}

impl Machine {
    /// Returns random registers, flags and RAM, with the stack holding a return address
    /// (or an exception frame) that leads to the sentinel.
    pub fn random(seed: u64, exception_frame: bool) -> Self {
        let mut random = SplitMix(seed);
        let ccr = (random.next_u64() & 0x1F) as u16;
        let mut cpu = Cpu::new(0x2700 | ccr, random.next_u64() as u32 & !1, STACK);
        for register in &mut cpu.d {
            *register = random.next_u64() as u32;
        }
        // Address registers point into RAM, where the game keeps its objects.
        for register in &mut cpu.a[..7] {
            *register = 0xFFFF_0000 | (random.next_u64() as u32 & 0xFFFE);
        }
        let mut ram = Box::new([0; RAM_SIZE]);
        for byte in ram.iter_mut() {
            *byte = random.next_u64() as u8;
        }
        let io = Io::new(random.next_u64());
        let mut machine = Self { cpu, ram, io };
        let sr = machine.cpu.sr().to_be_bytes();
        let mut frame = Vec::new();
        if exception_frame {
            frame.extend(sr);
        }
        frame.extend(SENTINEL.to_be_bytes());
        machine.cpu.a[7] -= frame.len() as u32;
        let base = machine.cpu.a[7] as usize;
        for (offset, byte) in frame.into_iter().enumerate() {
            machine.ram[(base + offset) & (RAM_SIZE - 1)] = byte;
        }
        machine
    }

    /// Splits the machine into its CPU and a bus over its memory.
    pub fn split<'a>(
        &'a mut self,
        rom: &'a [u8],
        watch: Option<&'a mut Watch>,
    ) -> (&'a mut Cpu, SystemBus<'a>) {
        let bus = SystemBus {
            ram: &mut self.ram,
            rom,
            device: &mut self.io,
            watch,
        };
        (&mut self.cpu, bus)
    }
}

/// Where a run starts: an address and the translated function that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Start {
    pub address: Address,
    pub function: Address,
}

/// How a run ended. The address is where control was going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stop {
    /// Returned to the sentinel.
    Returned,
    /// An indirect jump or call to a target the analysis does not list.
    OffTable(Address),
    /// A transfer to an address the analysis did not decode.
    LeftCode(Address),
    /// A transfer to decoded code that has no translated path from here; the analysis
    /// should never produce one.
    Dispatched(Address),
    /// A return to an address no frame expects, or that the table cannot resume at.
    OffStack(Address),
    /// STOP.
    Stopped,
    Cap,
}

impl Stop {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Returned => "returned",
            Self::OffTable(_) => "off table",
            Self::LeftCode(_) => "left code",
            Self::Dispatched(_) => "dispatched",
            Self::OffStack(_) => "off stack",
            Self::Stopped => "stopped",
            Self::Cap => "cap",
        }
    }
}

/// The end of a reference run.
#[derive(Debug)]
pub struct Outcome {
    pub stop: Stop,
    pub steps: u64,
    /// Returns the translated code must resume at its top level, in order.
    pub arrivals: Vec<Address>,
    /// The flags translated code must agree on.
    pub live: FlagSet,
    pub machine: Machine,
    /// The last instructions run, for failure messages.
    pub trail: Trail,
}

/// The addresses of the last instructions run.
#[derive(Debug, Default)]
pub struct Trail(VecDeque<Address>);

impl Trail {
    fn push(&mut self, address: Address) {
        if self.0.len() == TRAIL {
            self.0.pop_front();
        }
        self.0.push_back(address);
    }
}

impl std::fmt::Display for Trail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, " (after")?;
        for address in self.0.iter().skip(self.0.len().saturating_sub(TRAIL_SHOWN)) {
            write!(f, " {:06x}", address.get())?;
        }
        write!(f, ")")
    }
}

/// A call or exception the 68000 will return from.
#[derive(Debug, Clone, Copy)]
struct Frame {
    expected: Address,
    /// Whether translated code holds a native frame checking for `expected`: the
    /// continuation lies in the calling function.
    native: bool,
    function: Address,
}

/// Runs the reference side of function runs over one analysis.
pub struct Reference<'a> {
    analysis: &'a Analysis,
    liveness: &'a FlagLiveness<'a>,
    rom: &'a [u8],
    /// Every address the translated table resumes at, with the function that runs it.
    table: &'a BTreeMap<Address, Address>,
    /// The translated function of each exception vector, as the emitter calls it.
    handlers: BTreeMap<u8, Address>,
    overrides: BTreeMap<Address, Lifted>,
    interpreter: Interpreter,
}

impl<'a> Reference<'a> {
    pub fn new(
        analysis: &'a Analysis,
        liveness: &'a FlagLiveness<'a>,
        rom: &'a [u8],
        table: &'a BTreeMap<Address, Address>,
    ) -> Self {
        let mut handlers = BTreeMap::new();
        for function in analysis.functions.values() {
            if !analysis.blocks.contains_key(&function.entry) {
                continue;
            }
            for origin in &function.origins {
                if let Origin::Vector(vector) = origin {
                    handlers.insert(*vector, function.entry);
                }
            }
        }
        Self {
            analysis,
            liveness,
            rom,
            table,
            handlers,
            overrides: BTreeMap::new(),
            interpreter: Interpreter::default(),
        }
    }

    /// Runs `lifted` instead of the analysed instruction at its address; tests use it to
    /// check that a wrong reference is caught.
    pub fn with_override(mut self, lifted: Lifted) -> Self {
        self.overrides.insert(lifted.address, lifted);
        self
    }

    /// Returns whether `function` ends with RTE, so its runs start from an exception frame
    /// rather than a return address.
    pub fn returns_with_rte(&self, function: Address) -> bool {
        self.analysis.functions[&function]
            .blocks
            .iter()
            .any(|start| self.analysis.blocks[start].terminator == Terminator::ReturnFromException)
    }

    /// Runs from `start` with `machine` for at most `cap` instructions.
    ///
    /// # Errors
    /// Returns the first difference from Musashi, or a harness inconsistency.
    pub fn run(
        &mut self,
        start: Start,
        mut machine: Machine,
        cap: u64,
        mut lockstep: Option<&mut Lockstep>,
    ) -> Result<Outcome, String> {
        machine.cpu.pc = start.address;
        if let Some(lockstep) = lockstep.as_deref_mut() {
            lockstep.load(&machine);
        }
        let mut frames: Vec<Frame> = Vec::new();
        let mut current = start.function;
        let mut arrivals = Vec::new();
        let mut trail = Trail::default();
        for steps in 0..cap {
            let pc = machine.cpu.pc;
            let lifted = self
                .overrides
                .get(&pc)
                .or_else(|| self.liveness.lifted(pc))
                .ok_or_else(|| format!("no analysed instruction at {pc:06x}{trail}"))?;
            trail.push(pc);
            let mut watch = Watch::default();
            let step = {
                let (cpu, mut bus) = machine.split(self.rom, Some(&mut watch));
                self.interpreter.run(lifted, cpu, &mut bus)
            };
            if let Some(lockstep) = lockstep.as_deref_mut() {
                let instruction = &self.analysis.instructions[&pc];
                lockstep
                    .step(self.rom, instruction, &machine, &watch)
                    .map_err(|error| format!("Musashi: {error}{trail}"))?;
            }
            let transfer = step.outcome.transfer;
            let stop = match transfer {
                None => self.continue_at(&mut current, lifted.next()),
                Some(Transfer::Jump(target)) if !self.is_listed(lifted, target) => {
                    Some(Stop::OffTable(target))
                }
                Some(Transfer::Jump(target)) => self.continue_at(&mut current, target),
                Some(Transfer::Call { target, .. }) if !self.is_listed(lifted, target) => {
                    Some(Stop::OffTable(target))
                }
                Some(Transfer::Call {
                    target,
                    return_address,
                }) if self.translated(target) => {
                    frames.push(self.frame(current, return_address));
                    current = target;
                    None
                }
                Some(Transfer::Call { target, .. }) => Some(self.dispatched(target)),
                Some(Transfer::Exception { vector, pc: frame }) => {
                    match self.handlers.get(&vector) {
                        Some(&handler) => {
                            frames.push(self.frame(current, frame));
                            current = handler;
                            None
                        }
                        None => Some(self.dispatched(machine.cpu.pc)),
                    }
                }
                Some(Transfer::Return { target, .. }) => {
                    self.ret(&mut frames, &mut current, &mut arrivals, target)
                }
                Some(Transfer::Stop { .. }) => Some(Stop::Stopped),
                Some(other) => return Err(format!("unknown transfer {other:?}{trail}")),
            };
            if let Some(stop) = stop {
                let live = self.live_at(stop, start.function);
                return Ok(Outcome {
                    stop,
                    steps: steps + 1,
                    arrivals,
                    live,
                    machine,
                    trail,
                });
            }
        }
        Ok(Outcome {
            stop: Stop::Cap,
            steps: cap,
            arrivals,
            live: FlagSet::EMPTY,
            machine,
            trail,
        })
    }

    fn frame(&self, function: Address, expected: Address) -> Frame {
        Frame {
            expected,
            native: self.owns(function, expected),
            function,
        }
    }

    /// A return to `target`: unwinds to the innermost native frame expecting it, else
    /// arrives at the top level.
    fn ret(
        &self,
        frames: &mut Vec<Frame>,
        current: &mut Address,
        arrivals: &mut Vec<Address>,
        target: Address,
    ) -> Option<Stop> {
        if target.get() == SENTINEL {
            return Some(Stop::Returned);
        }
        let native = frames
            .iter()
            .rposition(|frame| frame.native && frame.expected == target);
        if let Some(depth) = native {
            *current = frames[depth].function;
            frames.truncate(depth);
            return None;
        }
        let any = frames.iter().rposition(|frame| frame.expected == target);
        match (any, self.table.get(&target)) {
            (Some(depth), Some(&function)) => {
                // The top level resumes with an empty native stack.
                frames.truncate(depth);
                for frame in frames.iter_mut() {
                    frame.native = false;
                }
                *current = function;
                arrivals.push(target);
                None
            }
            _ => Some(Stop::OffStack(target)),
        }
    }

    /// Continues at `target` the way the emitter does: a goto inside the function, a tail
    /// call to another function's entry, otherwise the dispatcher.
    fn continue_at(&self, current: &mut Address, target: Address) -> Option<Stop> {
        if self.owns(*current, target) {
            None
        } else if self.translated(target) {
            *current = target;
            None
        } else {
            Some(self.dispatched(target))
        }
    }

    fn dispatched(&self, target: Address) -> Stop {
        if self.analysis.instructions.contains_key(&target) {
            Stop::Dispatched(target)
        } else {
            Stop::LeftCode(target)
        }
    }

    /// Whether `function` has translated code for the instruction at `address`.
    fn owns(&self, function: Address, address: Address) -> bool {
        if !self.analysis.instructions.contains_key(&address) {
            return false;
        }
        let Some((start, block)) = self.analysis.blocks.range(..=address).next_back() else {
            return false;
        };
        address < block.end
            && self
                .analysis
                .functions
                .get(&function)
                .is_some_and(|function| function.blocks.contains(start))
    }

    /// Whether `entry` is a function with translated code.
    fn translated(&self, entry: Address) -> bool {
        self.analysis.functions.contains_key(&entry) && self.analysis.blocks.contains_key(&entry)
    }

    /// Returns whether a transfer by `lifted` to `target` goes where the analysis expects:
    /// always for direct transfers, and to a listed target for indirect ones.
    fn is_listed(&self, lifted: &Lifted, target: Address) -> bool {
        let Some((_, block)) = self.analysis.blocks.range(..=lifted.address).next_back() else {
            return false;
        };
        match lifted.body.ops.last() {
            Some(Op::Exit(Control::JumpIndirect(_))) => {
                block.successors.iter().any(|edge| edge.target == target)
            }
            Some(Op::Exit(Control::CallIndirect { .. })) => block
                .calls
                .iter()
                .any(|call| call.site == lifted.address && call.targets.contains(&target)),
            _ => true,
        }
    }

    /// The flags liveness promises exact where a run from `function` ended with `stop`,
    /// as in M2.3: what the function's return points need at the sentinel, and every flag
    /// where control leaves for the dispatcher or stops.
    ///
    /// Off the table or the stack nothing is promised. The live set of an instruction
    /// inside a callee is the union over all its callers, so it can hold flags this run's
    /// caller dropped (Sonic 1: X at the ObjPosLoad table jump, written dead in ColIndexLoad).
    fn live_at(&self, stop: Stop, function: Address) -> FlagSet {
        match stop {
            Stop::Returned => self
                .liveness
                .summary(function)
                .map_or(FlagSet::ALL, |summary| summary.return_live),
            Stop::OffTable(_) | Stop::OffStack(_) | Stop::Cap => FlagSet::EMPTY,
            Stop::LeftCode(_) | Stop::Dispatched(_) | Stop::Stopped => FlagSet::ALL,
        }
    }

    /// Lists the instructions of `trail` that write any of `flags`, with the writes pruning
    /// drops and the flags live after them; for failure messages.
    pub fn explain(&self, trail: &Trail, flags: FlagSet) -> String {
        let writes = |body: &anygenesis_ir::Body| -> FlagSet {
            body.ops
                .iter()
                .filter_map(|op| match op {
                    Op::SetFlag { flag, .. } => Some(FlagSet::of(*flag)),
                    _ => None,
                })
                .fold(FlagSet::EMPTY, |set, flag| set | flag)
                & flags
        };
        let names = |set: FlagSet| set.iter().collect::<Vec<_>>();
        let mut text = String::new();
        for &address in &trail.0 {
            let Some(lifted) = self.liveness.lifted(address) else {
                continue;
            };
            if let Some(Op::Exit(Control::Call { target, .. })) = lifted.body.ops.last()
                && let Some(summary) = self.liveness.summary(*target)
            {
                text.push_str(&format!(
                    "\n  {:06x} calls {:06x}: reads {:?}, defines {:?}, live before {:?}",
                    address.get(),
                    target.get(),
                    names(summary.reads),
                    names(summary.defines),
                    names(self.liveness.live_in(address).unwrap_or(FlagSet::ALL))
                ));
            }
            let written = writes(&lifted.body);
            if written == FlagSet::EMPTY {
                continue;
            }
            let dropped = written - writes(&self.liveness.prune(lifted).body);
            let after = self.liveness.live_out(address).unwrap_or(FlagSet::ALL);
            text.push_str(&format!(
                "\n  {:06x} writes {:?}, pruned {:?}, live after {:?}",
                address.get(),
                names(written),
                names(dropped),
                names(after)
            ));
        }
        if let Some(&last) = trail.0.back() {
            let live = self.liveness.live_in(last).unwrap_or(FlagSet::ALL);
            text.push_str(&format!(
                "\n  {:06x} last, live before {:?}",
                last.get(),
                names(live)
            ));
        }
        text
    }

    /// The addresses translated code can start at, with their functions.
    pub fn table(&self) -> &BTreeMap<Address, Address> {
        self.table
    }

    /// Every function with translated code, in address order.
    pub fn functions(&self) -> BTreeSet<Address> {
        self.analysis
            .functions
            .keys()
            .copied()
            .filter(|&entry| self.translated(entry))
            .collect()
    }
}
