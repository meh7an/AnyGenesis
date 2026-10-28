//! Every translated function of a real ROM, analysed from its vectors as game builds do,
//! against the interpreter, with Musashi in lockstep: from each function entry with 8 random
//! states and from each resume point with 2.
//!
//! Needs `ANYGENESIS_ROM` set when the crate is built (the build script translates the ROM)
//! and when the test runs. Run with `cargo test -p anygenesis-c-oracle --test rom --
//! --ignored --nocapture`; the report lands in target/tmp/rom-c-report.md.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use anygenesis_analysis::{FlagLiveness, LivenessOptions, analyze};
use anygenesis_c_oracle::{
    differential::{Differential, Table},
    lockstep::Lockstep,
    programs,
    reference::Stop,
};
use anygenesis_rom::Rom;
use anygenesis_shared::Address;

const ENTRY_SEEDS: u64 = 8;
const RESUME_SEEDS: u64 = 2;
const CAP: u64 = 20_000;
/// Translated calls nest on the native stack, and a debug build's frames are large.
const STACK: usize = 256 << 20;
const LISTED_FAILURES: usize = 20;

#[derive(Debug, Default)]
struct FunctionRuns {
    compared: u64,
    capped: u64,
    failed: u64,
}

#[test]
#[ignore = "needs ANYGENESIS_ROM when building and running"]
fn test_rom_functions_match_the_reference_cores() {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(run)
        .expect("the test thread starts")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn run() {
    let path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM names the ROM");
    let rom = Rom::load(&path).unwrap_or_else(|error| panic!("{error}"));
    let translated = programs::ROM_SHA1
        .expect("ANYGENESIS_ROM was not set when the crate was built: rebuild with it");
    assert_eq!(
        translated,
        rom.sha1().to_string(),
        "the ROM differs from the one the crate was built with"
    );
    let analysis = analyze(&rom);
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    let rows = programs::rom_table().expect("the ROM was translated");
    let table = Table::new(rows, &analysis);
    let mut differential = Differential::new(&analysis, &liveness, rom.bytes(), &table);
    let mut lockstep = Lockstep::new();

    let mut functions: BTreeMap<Address, FunctionRuns> = BTreeMap::new();
    let mut stops: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut failed = 0u64;
    for start in table.starts() {
        let entry = start.address == start.function;
        let seeds = if entry { ENTRY_SEEDS } else { RESUME_SEEDS };
        let origin = if entry { "entry" } else { "resume point" };
        for seed in 0..seeds {
            let seed_value = u64::from(start.address.get()) << 8 | seed;
            let result = differential.run(start, seed_value, CAP, Some(&mut lockstep));
            let runs = functions.entry(start.function).or_default();
            match result {
                Ok(Stop::Cap) => {
                    runs.capped += 1;
                    *stops.entry((origin, "cap")).or_default() += 1;
                }
                Ok(stop) => {
                    runs.compared += 1;
                    *stops.entry((origin, stop.kind())).or_default() += 1;
                }
                Err(error) => {
                    runs.failed += 1;
                    failed += 1;
                    if failures.len() < LISTED_FAILURES {
                        failures.push(format!(
                            "{:06x} (fn {:06x}) seed {seed}: {error}",
                            start.address.get(),
                            start.function.get()
                        ));
                    }
                }
            }
        }
    }

    let mut ineligible: Vec<(Address, &str)> = Vec::new();
    for &entry in analysis.functions.keys() {
        match functions.get(&entry) {
            None => ineligible.push((entry, "entry not decoded: no translated code")),
            Some(runs) if runs.compared == 0 && runs.failed == 0 => {
                ineligible.push((
                    entry,
                    "every run hit the cap (endless loop or hardware wait)",
                ));
            }
            Some(_) => {}
        }
    }

    let mut report = String::new();
    let _ = writeln!(
        report,
        "ROM SHA-1 {}: {} functions, {} table rows, {} translated functions run.\n",
        rom.sha1(),
        analysis.functions.len(),
        table.owners.len(),
        functions.len()
    );
    report.push_str("| Start | Stop | Runs |\n|-------|------|-----:|\n");
    for ((origin, kind), count) in &stops {
        let _ = writeln!(report, "| {origin} | {kind} | {count} |");
    }
    let _ = writeln!(
        report,
        "\nFailures: {failed}. Musashi lockstep: {} instructions compared, masked: {:?}.",
        lockstep.compared, lockstep.masked
    );
    let _ = writeln!(report, "\nIneligible functions ({}):\n", ineligible.len());
    for (entry, reason) in &ineligible {
        let _ = writeln!(report, "- {:06x}: {reason}", entry.get());
    }
    if !failures.is_empty() {
        report.push_str("\nFirst failures:\n\n");
        for failure in &failures {
            let _ = writeln!(report, "- {failure}");
        }
    }
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rom-c-report.md");
    fs::write(&output, &report).expect("writing the report");
    println!("{report}\nreport written to {}", output.display());
    assert_eq!(failed, 0, "{failed} runs failed; see {}", output.display());
}
