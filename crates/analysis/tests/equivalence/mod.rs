//! Liveness on against liveness off: runs a function twice in the IR interpreter from one
//! random state, once with the lifted bodies and once with the pruned ones, in lockstep.
//!
//! After every instruction the registers, the transfer and every bus access (address, value,
//! order) must agree, which covers each place flags reach the rest of the machine: branch
//! conditions, MOVE from SR, the status register an exception frame pushes. Flags are
//! compared where the run ends: at the sentinel return address the flags the function's
//! return points need, on leaving the analysed code or at STOP every flag. Runs also end on
//! an indirect transfer to a target the analysis does not list (random state indexes tables
//! out of range), on a return no call frame expects (random state can overwrite the stack),
//! or at the step cap.
//!
//! The live set of a single instruction is not compared: in a function with several
//! callers it is the union over all of them, so it can hold flags the running caller
//! legitimately dropped.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
};

use anygenesis_analysis::{Analysis, FlagLiveness, FlagStats, Terminator};
use anygenesis_ir::{
    Control, Flag, FlagSet, Lifted, Op,
    interp::{Cpu, Interpreter, Transfer},
    validate,
};
use anygenesis_shared::{Address, Bus, Size};

/// Where a run returns to; outside the ROM and RAM, so no analysed code lives there.
const SENTINEL: u32 = 0x00F0_0000;
/// Sonic 1's initial supervisor stack pointer.
const STACK: u32 = 0x00FF_FE00;
const RAM_START: u32 = 0xE0_0000;
const RAM_SIZE: usize = 0x1_0000;
const ROM_END: u32 = 0x40_0000;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stop {
    Returned,
    LeftCode,
    Stopped,
    OffTable,
    OffStack,
    Cap,
}

/// One run that stayed equivalent.
#[derive(Debug)]
pub struct Run {
    pub steps: u64,
    pub stop: Stop,
    /// Flag ops of the executed instructions, and how many the pruned bodies skipped.
    pub executed: FlagStats,
}

/// Runs liveness-on against liveness-off for the functions of one analysis.
pub struct Harness<'a> {
    analysis: &'a Analysis,
    liveness: &'a FlagLiveness<'a>,
    rom: &'a [u8],
    pruned: BTreeMap<Address, Lifted>,
    interpreter: Interpreter,
}

impl<'a> Harness<'a> {
    pub fn new(analysis: &'a Analysis, liveness: &'a FlagLiveness<'a>, rom: &'a [u8]) -> Self {
        Self::with_prune(analysis, liveness, rom, |lifted| liveness.prune(lifted))
    }

    /// Returns a harness whose liveness-on side runs the bodies `prune` gives; tests use it
    /// to check that a wrong pruning is caught.
    pub fn with_prune(
        analysis: &'a Analysis,
        liveness: &'a FlagLiveness<'a>,
        rom: &'a [u8],
        prune: impl Fn(&Lifted) -> Lifted,
    ) -> Self {
        let pruned = analysis
            .instructions
            .keys()
            .filter_map(|&address| liveness.lifted(address))
            .map(|lifted| {
                let pruned = prune(lifted);
                assert_eq!(
                    validate(&pruned.body),
                    Ok(()),
                    "pruned {:?}",
                    lifted.address
                );
                (lifted.address, pruned)
            })
            .collect();
        Self {
            analysis,
            liveness,
            rom,
            pruned,
            interpreter: Interpreter::default(),
        }
    }

    /// Runs the function at `entry` from the random state `seed` for at most `cap` steps.
    ///
    /// # Errors
    /// Returns a description of the first difference.
    pub fn run(&mut self, entry: Address, seed: u64, cap: u64) -> Result<Run, String> {
        let mut off = Machine::new(self.rom, seed, self.returns_with_rte(entry));
        off.cpu.pc = entry;
        let mut on = off.clone();
        let mut executed = FlagStats::default();
        let return_live = self
            .liveness
            .summary(entry)
            .map_or(FlagSet::ALL, |summary| summary.return_live);
        // The return addresses the calls and exceptions so far expect, innermost last.
        let mut frames = vec![Address::new(SENTINEL)];
        let mut trail = Trail::default();
        for steps in 0..cap {
            let pc = off.cpu.pc;
            if pc.get() == SENTINEL {
                compare_flags(&off, &on, return_live, "at the return", &trail)?;
                return Ok(Run {
                    steps,
                    stop: Stop::Returned,
                    executed,
                });
            }
            let Some(original) = self.liveness.lifted(pc) else {
                let place = "on leaving the analysed code";
                compare_flags(&off, &on, FlagSet::ALL, place, &trail)?;
                return Ok(Run {
                    steps,
                    stop: Stop::LeftCode,
                    executed,
                });
            };
            trail.push(pc);
            let pruned = &self.pruned[&pc];
            executed.add(&original.body, &pruned.body);
            let off_step = self.interpreter.run(original, &mut off.cpu, &mut off.bus);
            let on_step = self.interpreter.run(pruned, &mut on.cpu, &mut on.bus);
            if off_step.outcome != on_step.outcome {
                return Err(format!(
                    "outcome {:?} against {:?}{trail}",
                    off_step.outcome, on_step.outcome
                ));
            }
            compare_state(&off, &on).map_err(|error| format!("{error}{trail}"))?;
            off.bus.accesses.clear();
            on.bus.accesses.clear();
            let stop = match off_step.outcome.transfer {
                Some(Transfer::Stop { .. }) => {
                    compare_flags(&off, &on, FlagSet::ALL, "at STOP", &trail)?;
                    Some(Stop::Stopped)
                }
                Some(Transfer::Jump(target) | Transfer::Call { target, .. })
                    if !self.is_listed_target(original, target) =>
                {
                    Some(Stop::OffTable)
                }
                Some(Transfer::Call { return_address, .. }) => {
                    frames.push(return_address);
                    None
                }
                Some(Transfer::Exception { pc, .. }) => {
                    frames.push(pc);
                    None
                }
                // A return to an outer frame is a popped caller; to no frame, a stack the
                // random state overwrote.
                Some(Transfer::Return { target, .. }) => {
                    match frames.iter().rposition(|&frame| frame == target) {
                        Some(depth) => {
                            frames.truncate(depth);
                            None
                        }
                        None => Some(Stop::OffStack),
                    }
                }
                _ => None,
            };
            if let Some(stop) = stop {
                return Ok(Run {
                    steps: steps + 1,
                    stop,
                    executed,
                });
            }
        }
        Ok(Run {
            steps: cap,
            stop: Stop::Cap,
            executed,
        })
    }

    /// Returns whether a transfer by `lifted` to `target` goes where the analysis expects:
    /// always for direct transfers, and to a listed target for indirect ones.
    fn is_listed_target(&self, lifted: &Lifted, target: Address) -> bool {
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

    /// Returns whether the function ends with RTE, so the run starts from an exception
    /// frame rather than a return address.
    fn returns_with_rte(&self, entry: Address) -> bool {
        self.analysis.functions[&entry]
            .blocks
            .iter()
            .any(|start| self.analysis.blocks[start].terminator == Terminator::ReturnFromException)
    }
}

/// The addresses of the last instructions run, for failure messages.
#[derive(Debug, Default)]
struct Trail(VecDeque<Address>);

impl Trail {
    const LENGTH: usize = 12;

    fn push(&mut self, address: Address) {
        if self.0.len() == Self::LENGTH {
            self.0.pop_front();
        }
        self.0.push_back(address);
    }
}

impl fmt::Display for Trail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, " (after")?;
        for address in &self.0 {
            write!(f, " {:06x}", address.get())?;
        }
        write!(f, ")")
    }
}

fn compare_flags(
    off: &Machine,
    on: &Machine,
    live: FlagSet,
    place: &str,
    trail: &Trail,
) -> Result<(), String> {
    let differing: Vec<Flag> = live
        .iter()
        .filter(|&flag| off.cpu.flag(flag) != on.cpu.flag(flag))
        .collect();
    if differing.is_empty() {
        Ok(())
    } else {
        Err(format!("live flags {differing:?} differ {place}{trail}"))
    }
}

fn compare_state(off: &Machine, on: &Machine) -> Result<(), String> {
    let (a, b) = (&off.cpu, &on.cpu);
    if a.d != b.d || a.a != b.a || a.inactive_sp != b.inactive_sp || a.pc != b.pc {
        return Err(format!("registers {a:x?} against {b:x?}"));
    }
    let system = |cpu: &Cpu| cpu.sr() & 0xFF00;
    if system(a) != system(b) {
        return Err(format!("SR {:04x} against {:04x}", a.sr(), b.sr()));
    }
    if off.bus.accesses != on.bus.accesses {
        return Err(format!(
            "bus accesses {:x?} against {:x?}",
            off.bus.accesses, on.bus.accesses
        ));
    }
    Ok(())
}

/// The CPU and memory of one side of a run.
#[derive(Debug, Clone)]
struct Machine<'a> {
    cpu: Cpu,
    bus: Memory<'a>,
}

impl<'a> Machine<'a> {
    /// Returns random registers, flags and RAM, with the stack holding a return address
    /// (or an exception frame) that leads to the sentinel.
    fn new(rom: &'a [u8], seed: u64, exception_frame: bool) -> Self {
        let mut random = SplitMix(seed);
        let ccr = (random.next() & 0x1F) as u16;
        let mut cpu = Cpu::new(0x2700 | ccr, random.next() as u32 & !1, STACK);
        for register in &mut cpu.d {
            *register = random.next() as u32;
        }
        // Address registers point into RAM, where the game keeps its objects.
        for register in &mut cpu.a[..7] {
            *register = 0xFFFF_0000 | (random.next() as u32 & 0xFFFE);
        }
        let ram = (0..RAM_SIZE).map(|_| random.next() as u8).collect();
        let mut bus = Memory {
            rom,
            ram,
            io: SplitMix(random.next()),
            accesses: Vec::new(),
        };
        if exception_frame {
            cpu.a[7] -= 6;
            bus.write_word(Address::new(cpu.a[7]), cpu.sr());
            bus.write_long(Address::new(cpu.a[7] + 2), SENTINEL);
        } else {
            cpu.a[7] -= 4;
            bus.write_long(Address::new(cpu.a[7]), SENTINEL);
        }
        bus.accesses.clear();
        Self { cpu, bus }
    }
}

/// One bus access: kind (read or write, byte or word), address and value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Access {
    write: bool,
    size: Size,
    address: u32,
    value: u32,
}

/// ROM, 64 KiB of RAM mirrored from 0xE00000, and I/O that reads a seeded random stream.
#[derive(Debug, Clone)]
struct Memory<'a> {
    rom: &'a [u8],
    ram: Vec<u8>,
    io: SplitMix,
    accesses: Vec<Access>,
}

impl Memory<'_> {
    fn log(&mut self, write: bool, size: Size, address: Address, value: u32) {
        self.accesses.push(Access {
            write,
            size,
            address: address.get(),
            value,
        });
    }

    fn byte(&mut self, address: u32) -> u8 {
        if address >= RAM_START {
            self.ram[(address as usize) & (RAM_SIZE - 1)]
        } else if address < ROM_END {
            self.rom.get(address as usize).copied().unwrap_or(0)
        } else {
            self.io.next() as u8
        }
    }

    fn store(&mut self, address: u32, value: u8) {
        if address >= RAM_START {
            self.ram[(address as usize) & (RAM_SIZE - 1)] = value;
        }
    }
}

impl Bus for Memory<'_> {
    fn read_byte(&mut self, address: Address) -> u8 {
        let value = self.byte(address.get());
        self.log(false, Size::Byte, address, value.into());
        value
    }

    fn read_word(&mut self, address: Address) -> u16 {
        let high = self.byte(address.get());
        let low = self.byte(address.wrapping_add(1).get());
        let value = u16::from_be_bytes([high, low]);
        self.log(false, Size::Word, address, value.into());
        value
    }

    fn write_byte(&mut self, address: Address, value: u8) {
        self.log(true, Size::Byte, address, value.into());
        self.store(address.get(), value);
    }

    fn write_word(&mut self, address: Address, value: u16) {
        self.log(true, Size::Word, address, value.into());
        let [high, low] = value.to_be_bytes();
        self.store(address.get(), high);
        self.store(address.wrapping_add(1).get(), low);
    }
}

/// SplitMix64, enough randomness for test states without a dependency.
#[derive(Debug, Clone)]
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}
