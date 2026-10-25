//! Flag liveness over the real ROM named by `ANYGENESIS_ROM`, analysed with
//! `games/sonic1.toml`: liveness on against liveness off for every function from random
//! states, and the flag op removal statistics, written to
//! `target/tmp/flag-liveness-report.md`. Run with
//! `cargo test -p anygenesis-analysis --test liveness_rom -- --ignored --nocapture`.

mod equivalence;

use std::{collections::BTreeMap, fmt::Write as _, path::Path};

use anygenesis_analysis::{FlagLiveness, FlagStats, LivenessOptions, analyze};
use anygenesis_gamecfg::GameConfig;
use anygenesis_ir::Flag;
use anygenesis_rom::Rom;

use equivalence::{Harness, SplitMix, Stop};

const SEEDS: u64 = 8;
/// Enough for most functions to return; hardware wait loops and frame loops stop here.
const CAP: u64 = 20_000;
/// Distinct from the synthetic tests' seeds.
const SEED_BASE: u64 = 0x5EED_0001;

#[test]
#[ignore = "needs ANYGENESIS_ROM pointing at the Sonic 1 REV00 .bin"]
fn test_liveness_real_rom_sonic1_is_equivalent_and_reports_removals() {
    let path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is not set");
    let rom = Rom::load(&path).expect("ANYGENESIS_ROM loads");
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/sonic1.toml");
    let text = std::fs::read_to_string(&config_path).expect("games/sonic1.toml is readable");
    let config = match GameConfig::parse(&text) {
        Ok(config) => config,
        Err(error) => panic!("games/sonic1.toml:\n{error}"),
    };
    let analysis = match analyze(&rom, &config) {
        Ok(analysis) => analysis,
        Err(error) => panic!("{error}"),
    };
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    let checked = FlagLiveness::new(
        &analysis,
        LivenessOptions {
            check_points_read_sr: true,
        },
    );

    let mut harness = Harness::new(&analysis, &liveness, rom.bytes());
    let mut seeds = SplitMix(SEED_BASE);
    let mut stops: BTreeMap<Stop, u64> = BTreeMap::new();
    let mut executed = FlagStats::default();
    let mut steps = 0;
    let mut failures = Vec::new();
    for &entry in analysis.functions.keys() {
        for _ in 0..SEEDS {
            let seed = seeds.next();
            match harness.run(entry, seed, CAP) {
                Ok(run) => {
                    *stops.entry(run.stop).or_default() += 1;
                    steps += run.steps;
                    executed.merge(&run.executed);
                }
                Err(error) => failures.push(format!("{:06x} seed {seed:#x}: {error}", entry.get())),
            }
        }
    }

    let unknown_callers = analysis
        .functions
        .keys()
        .filter(|&&entry| {
            liveness
                .summary(entry)
                .is_some_and(|summary| summary.unknown_callers)
        })
        .count();
    let mut report = String::new();
    writeln!(report, "# Flag liveness: Sonic 1\n").unwrap();
    writeln!(report, "| Item | Count |\n|------|------:|").unwrap();
    writeln!(report, "| Functions | {} |", analysis.functions.len()).unwrap();
    writeln!(report, "| Instructions | {} |", analysis.instructions.len()).unwrap();
    writeln!(
        report,
        "| Functions with unknown callers | {unknown_callers} |"
    )
    .unwrap();
    writeln!(
        report,
        "| Unbalanced RTS | {} |",
        liveness.unbalanced_returns().count()
    )
    .unwrap();
    writeln!(report).unwrap();
    write_stats(&mut report, "Static, default", &liveness.stats());
    write_stats(
        &mut report,
        "Static, check points read SR",
        &checked.stats(),
    );
    write_stats(&mut report, "Executed in the equivalence runs", &executed);
    writeln!(report, "## Runs\n").unwrap();
    writeln!(
        report,
        "{} functions x {SEEDS} seeds, cap {CAP} steps, {steps} steps in all.\n",
        analysis.functions.len()
    )
    .unwrap();
    writeln!(report, "| Stop | Runs |\n|------|-----:|").unwrap();
    for (stop, count) in &stops {
        writeln!(report, "| {stop:?} | {count} |").unwrap();
    }
    writeln!(report, "\nFailures: {}", failures.len()).unwrap();
    for failure in failures.iter().take(20) {
        writeln!(report, "- {failure}").unwrap();
    }

    let report_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("flag-liveness-report.md");
    std::fs::write(&report_path, &report).expect("report is writable");
    println!("{report}");
    println!("report {}", report_path.display());

    assert!(failures.is_empty(), "{} runs diverged", failures.len());
}

fn write_stats(report: &mut String, title: &str, stats: &FlagStats) {
    writeln!(report, "## {title}\n").unwrap();
    writeln!(report, "| Flag | SetFlag ops | Removed | Removed % |").unwrap();
    writeln!(report, "|------|------------:|--------:|----------:|").unwrap();
    let rows = Flag::ALL
        .iter()
        .enumerate()
        .map(|(index, flag)| (format!("{flag:?}"), stats.ops[index], stats.removed[index]));
    let total = ("All".to_owned(), stats.total_ops(), stats.total_removed());
    for (name, ops, removed) in rows.chain([total]) {
        let percent = if ops == 0 {
            0.0
        } else {
            100.0 * removed as f64 / ops as f64
        };
        writeln!(report, "| {name} | {ops} | {removed} | {percent:.1} |").unwrap();
    }
    writeln!(report).unwrap();
}
