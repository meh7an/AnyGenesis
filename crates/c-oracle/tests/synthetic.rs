//! The synthetic program through translated C against the interpreter, and against Musashi
//! in lockstep, from every entry and resume point. Runs in every `cargo test`.

use std::collections::BTreeMap;

use anygenesis_analysis::{Analysis, FlagLiveness, LivenessOptions};
use anygenesis_c_oracle::{
    differential::{Differential, Table},
    lockstep::Lockstep,
    programs,
    reference::{Reference, Stop},
    synthetic,
};
use anygenesis_ir::lift;
use anygenesis_m68k::decode;
use anygenesis_shared::Address;

const SEEDS: u64 = 64;
const CAP: u64 = 2_000;
const SPIN: u32 = 0x700;

struct Program {
    analysis: Analysis,
    rom: Vec<u8>,
}

impl Program {
    fn new() -> Self {
        Self {
            analysis: synthetic::analysis(),
            rom: synthetic::image(),
        }
    }
}

/// Runs every start with every seed; returns the stop counts by kind.
fn run_all(
    differential: &mut Differential<'_>,
    table: &Table,
    mut lockstep: Option<&mut Lockstep>,
) -> BTreeMap<&'static str, u64> {
    let mut stops = BTreeMap::new();
    for start in table.starts() {
        for seed in 0..SEEDS {
            let stop = differential
                .run(start, seed, CAP, lockstep.as_deref_mut())
                .unwrap_or_else(|error| {
                    panic!("start {:06x} seed {seed}: {error}", start.address.get())
                });
            if start.function.get() == SPIN {
                assert_eq!(stop, Stop::Cap, "the spin function never returns");
            }
            *stops.entry(stop.kind()).or_default() += 1;
        }
    }
    stops
}

#[test]
fn test_synthetic_program_matches_the_interpreter() {
    let program = Program::new();
    let liveness = FlagLiveness::new(&program.analysis, LivenessOptions::default());
    let table = Table::new(programs::synthetic_table(), &program.analysis);
    assert!(
        table.owners.len() > program.analysis.functions.len(),
        "resume points are in the table"
    );
    let mut differential = Differential::new(&program.analysis, &liveness, &program.rom, &table);
    let stops = run_all(&mut differential, &table, None);
    // The table arms, the popped caller and the polling loop all end at the sentinel or
    // the cap; random returns from resume points go off the stack.
    for kind in ["returned", "cap", "off stack"] {
        assert!(stops.contains_key(kind), "no run {kind}: {stops:?}");
    }
}

#[test]
fn test_synthetic_program_matches_musashi_in_lockstep() {
    let program = Program::new();
    let liveness = FlagLiveness::new(&program.analysis, LivenessOptions::default());
    let table = Table::new(programs::synthetic_table(), &program.analysis);
    let mut differential = Differential::new(&program.analysis, &liveness, &program.rom, &table);
    let mut lockstep = Lockstep::new();
    run_all(&mut differential, &table, Some(&mut lockstep));
    assert!(lockstep.compared > 0);
}

/// Guards the comparison itself: a reference that adds 2 where the code adds 1 must fail.
#[test]
fn test_differential_catches_a_wrong_reference() {
    let program = Program::new();
    let liveness = FlagLiveness::new(&program.analysis, LivenessOptions::default());
    let table = Table::new(programs::synthetic_table(), &program.analysis);
    let arm = Address::new(0x31A);
    let addq_2 = decode(&[0x5442]).expect("addq.w #2,d2 decodes");
    let wrong = lift(&addq_2, arm).expect("addq lifts");
    let reference = Reference::new(&program.analysis, &liveness, &program.rom, &table.owners)
        .with_override(wrong);
    let mut differential = Differential::with_reference(reference, &table, &program.rom);
    let reset = table
        .starts()
        .find(|start| start.address.get() == 0x300)
        .expect("reset is a start");
    let results: Vec<Result<Stop, String>> = (0..SEEDS)
        .map(|seed| differential.run(reset, seed, CAP, None))
        .collect();
    assert!(
        results.iter().any(Result::is_err),
        "no run noticed the wrong addq: {results:?}"
    );
}
