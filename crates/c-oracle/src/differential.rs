//! Function-level differential: the reference run, then the translated code from the same
//! state, then a comparison of where and how both ended.

use std::collections::BTreeMap;

use anygenesis_abi::{AgEntry, AgFn, AgState};
use anygenesis_analysis::{Analysis, FlagLiveness};
use anygenesis_ir::Flag;
use anygenesis_shared::Address;

use crate::{
    bus::{Device, Io},
    host::{self, Host, POISON},
    lockstep::Lockstep,
    reference::{Machine, Outcome, Reference, SENTINEL, Start, Stop},
};

/// The translated address table: every function entry and resume point.
#[derive(Debug)]
pub struct Table {
    pub functions: BTreeMap<Address, AgFn>,
    /// The entry of the function that runs each address.
    pub owners: BTreeMap<Address, Address>,
}

impl Table {
    /// Reads the generated table of `analysis`'s translated code.
    ///
    /// # Panics
    /// Panics when a row names an address no translated function owns: the table and the
    /// analysis do not belong together.
    pub fn new(entries: &[AgEntry], analysis: &Analysis) -> Self {
        let functions: BTreeMap<Address, AgFn> = entries
            .iter()
            .map(|entry| (Address::new(entry.address), entry.function))
            .collect();
        // As the emitter builds the table: an entry runs itself, a resume point runs in the
        // lowest function owning its block.
        let owners = functions
            .keys()
            .map(|&address| {
                let owner = if analysis.functions.contains_key(&address) {
                    Some(address)
                } else {
                    analysis
                        .blocks
                        .range(..=address)
                        .next_back()
                        .and_then(|(_, block)| block.functions.first().copied())
                };
                let owner = owner.unwrap_or_else(|| panic!("no function owns {address:06x}"));
                (address, owner)
            })
            .collect();
        Self { functions, owners }
    }

    /// Every run start the table offers: each function entry and each resume point.
    pub fn starts(&self) -> impl Iterator<Item = Start> + '_ {
        self.owners
            .iter()
            .map(|(&address, &function)| Start { address, function })
    }
}

/// How the translated code ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CStop {
    Returned,
    Lookup(Address),
    Arrival(Address),
    Stopped,
    Cap,
}

/// The host of a translated function run.
struct FunctionHost<'a> {
    rom: &'a [u8],
    io: Io,
    stop: Option<CStop>,
    checks: u64,
    cap: u64,
}

impl FunctionHost<'_> {
    fn end(&mut self, stop: CStop) -> u32 {
        self.stop.get_or_insert(stop);
        POISON
    }
}

impl Host for FunctionHost<'_> {
    fn memory(&mut self) -> (&[u8], &mut dyn Device) {
        (self.rom, &mut self.io)
    }

    fn lookup(&mut self, address: Address) -> AgFn {
        self.end(CStop::Lookup(address));
        host::poisoned
    }

    fn stop(&mut self, _resume: Address) -> u32 {
        self.end(CStop::Stopped)
    }

    fn check(&mut self, state: &mut AgState, pc: Address) -> u32 {
        // The budget stays spent, so every check point calls back and is counted.
        state.cycles = 0;
        self.checks += 1;
        if self.checks > self.cap {
            self.end(CStop::Cap)
        } else {
            pc.get()
        }
    }

    fn reset_devices(&mut self) {}
}

/// Runs reference and translated code from the same random states.
pub struct Differential<'a> {
    reference: Reference<'a>,
    table: &'a Table,
    rom: &'a [u8],
}

impl<'a> Differential<'a> {
    pub fn new(
        analysis: &'a Analysis,
        liveness: &'a FlagLiveness<'a>,
        rom: &'a [u8],
        table: &'a Table,
    ) -> Self {
        Self::with_reference(
            Reference::new(analysis, liveness, rom, &table.owners),
            table,
            rom,
        )
    }

    /// A differential against a given reference, which tests alter on purpose.
    pub fn with_reference(reference: Reference<'a>, table: &'a Table, rom: &'a [u8]) -> Self {
        Self {
            reference,
            table,
            rom,
        }
    }

    /// Runs from `start` with the random state `seed`, at most `cap` reference steps.
    ///
    /// # Errors
    /// Returns a description of the first difference.
    pub fn run(
        &mut self,
        start: Start,
        seed: u64,
        cap: u64,
        lockstep: Option<&mut Lockstep>,
    ) -> Result<Stop, String> {
        let initial = Machine::random(seed, self.reference.returns_with_rte(start.function));
        let outcome = self.reference.run(start, initial.clone(), cap, lockstep)?;
        if outcome.stop == Stop::Cap {
            return Ok(Stop::Cap);
        }
        let mut state = host::new_state();
        load(&mut state, &initial, self.rom);
        let mut host = FunctionHost {
            rom: self.rom,
            io: initial.io.clone(),
            stop: None,
            checks: 0,
            // Every check point starts a block: a correct run passes fewer than the
            // reference ran instructions.
            cap: outcome.steps + 1,
        };
        let stop = self.run_c(&mut state, &mut host, start, &outcome.arrivals);
        let trail = &outcome.trail;
        if !corresponds(outcome.stop, stop) {
            return Err(format!(
                "the reference ended {:?}, the translated code {stop:?}{trail}",
                outcome.stop
            ));
        }
        compare(&outcome, &state, &host.io).map_err(|error| {
            let liveness = self.reference.explain(trail, outcome.live);
            format!("{error} on ending {:?}{trail}{liveness}", outcome.stop)
        })?;
        Ok(outcome.stop)
    }

    fn run_c(
        &self,
        state: &mut AgState,
        host: &mut FunctionHost<'_>,
        start: Start,
        arrivals: &[Address],
    ) -> CStop {
        let mut arrivals = arrivals.iter();
        let mut pc = start.address;
        loop {
            let function = self.table.functions[&pc];
            let next = host::call(state, host, function, pc);
            if next == SENTINEL {
                return CStop::Returned;
            }
            if next & POISON != 0 {
                // Only the host hands out the poison, and it records why first.
                return host.stop.unwrap_or(CStop::Cap);
            }
            let address = Address::new(next);
            if arrivals.next() != Some(&address) || !self.table.functions.contains_key(&address) {
                return CStop::Arrival(address);
            }
            pc = address;
        }
    }
}

fn corresponds(reference: Stop, translated: CStop) -> bool {
    match (reference, translated) {
        (Stop::Returned, CStop::Returned) | (Stop::Stopped, CStop::Stopped) => true,
        (
            Stop::OffTable(target) | Stop::LeftCode(target) | Stop::Dispatched(target),
            CStop::Lookup(address),
        )
        | (Stop::OffStack(target), CStop::Arrival(address)) => target == address,
        _ => false,
    }
}

/// Copies the machine into the translated code's state.
fn load(state: &mut AgState, machine: &Machine, rom: &[u8]) {
    let cpu = &machine.cpu;
    state.rom = rom.as_ptr();
    // The ROM loader caps images at 4 MiB.
    state.rom_size = rom.len() as u32;
    state.d = cpu.d;
    state.a = cpu.a;
    state.inactive_sp = cpu.inactive_sp;
    state.sr = cpu.sr() & 0xFF00;
    for flag in Flag::ALL {
        *flag_field(state, flag) = cpu.flag(flag).into();
    }
    state.ram.copy_from_slice(&machine.ram[..]);
    state.cycles = 0;
}

fn flag_value(state: &AgState, flag: Flag) -> u8 {
    match flag {
        Flag::X => state.x,
        Flag::N => state.n,
        Flag::Z => state.z,
        Flag::V => state.v,
        Flag::C => state.c,
    }
}

fn flag_field(state: &mut AgState, flag: Flag) -> &mut u8 {
    match flag {
        Flag::X => &mut state.x,
        Flag::N => &mut state.n,
        Flag::Z => &mut state.z,
        Flag::V => &mut state.v,
        Flag::C => &mut state.c,
    }
}

fn compare(outcome: &Outcome, state: &AgState, io: &Io) -> Result<(), String> {
    let cpu = &outcome.machine.cpu;
    if state.d != cpu.d || state.a != cpu.a || state.inactive_sp != cpu.inactive_sp {
        return Err(format!(
            "registers d {:x?} a {:x?} isp {:x} against the reference's d {:x?} a {:x?} isp {:x}",
            state.d, state.a, state.inactive_sp, cpu.d, cpu.a, cpu.inactive_sp
        ));
    }
    if state.sr != cpu.sr() & 0xFF00 {
        return Err(format!(
            "SR system byte {:04x} against {:04x}",
            state.sr,
            cpu.sr() & 0xFF00
        ));
    }
    let differing: Vec<Flag> = outcome
        .live
        .iter()
        .filter(|&flag| (flag_value(state, flag) != 0) != cpu.flag(flag))
        .collect();
    if !differing.is_empty() {
        return Err(format!("live flags {differing:?} differ"));
    }
    if let Some(index) = (0..state.ram.len()).find(|&i| state.ram[i] != outcome.machine.ram[i]) {
        return Err(format!(
            "RAM {index:04x} holds {:02x} against the reference's {:02x}",
            state.ram[index], outcome.machine.ram[index]
        ));
    }
    if io.log != outcome.machine.io.log {
        let index = io
            .log
            .iter()
            .zip(&outcome.machine.io.log)
            .position(|(a, b)| a != b)
            .unwrap_or(io.log.len().min(outcome.machine.io.log.len()));
        return Err(format!(
            "I/O access {index}: {:x?} against the reference's {:x?}",
            io.log.get(index),
            outcome.machine.io.log.get(index)
        ));
    }
    Ok(())
}
