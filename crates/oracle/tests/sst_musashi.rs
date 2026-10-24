//! Runs the whole SingleStepTests/m68000 suite through Musashi and reports the pass rate per
//! instruction file. A case passes when registers, SR and RAM match; write order and cycle
//! counts are tabulated but never fail a case.
//!
//! Needs `cargo xtask fetch-sst`. Run with
//! `cargo test -p anygenesis-oracle --test sst_musashi -- --ignored --nocapture`.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anygenesis_oracle::{
    Registers,
    musashi::{AccessKind, Musashi},
    sst::{self, TestCase, Transaction, TransactionKind},
};

/// SingleStepTests stores PC as MAME's prefetch address, this far past the opcode. A
/// stopped CPU has not prefetched, so its stored PC is the real one.
const PC_OFFSET: u32 = 4;

/// Upstream flags these files as unreliable.
const UNRELIABLE: [&str; 2] = ["TAS.json.bin", "TRAPV.json.bin"];

const SR_FLAGS: [(u32, &str); 5] = [
    (0x10, "SR.X"),
    (0x08, "SR.N"),
    (0x04, "SR.Z"),
    (0x02, "SR.V"),
    (0x01, "SR.C"),
];

/// Failing case names kept per file, as starting points for the divergence list.
const SAMPLE_FAILURES: usize = 3;

/// Address-error cases are counted apart: their outcome is the exception stack frame, which
/// M2.2 excludes, and they would otherwise hide the semantic divergences.
#[derive(Debug, Default)]
struct FileReport {
    cases: usize,
    passed: usize,
    address_errors: usize,
    address_errors_passed: usize,
    write_matches: usize,
    cycle_matches: usize,
    /// Mismatch counts per state field, across the cases without an address error.
    mismatches: BTreeMap<&'static str, usize>,
    sample_failures: Vec<String>,
}

impl FileReport {
    fn add(&mut self, other: &Self) {
        self.cases += other.cases;
        self.passed += other.passed;
        self.address_errors += other.address_errors;
        self.address_errors_passed += other.address_errors_passed;
        self.write_matches += other.write_matches;
        self.cycle_matches += other.cycle_matches;
    }
}

#[test]
#[ignore = "needs testdata/m68000 from `cargo xtask fetch-sst`"]
fn test_sst_suite_through_musashi_reports_pass_rates() {
    let directory = suite_directory();
    let mut files: Vec<PathBuf> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()))
        .map(|entry| entry.expect("listing the suite directory").path())
        .filter(|path| path.to_string_lossy().ends_with(".json.bin"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no .json.bin files in {}",
        directory.display()
    );

    let mut core = Musashi::lock();
    let reports: Vec<(String, FileReport)> = files
        .iter()
        .map(|path| (file_name(path), run_file(&mut core, path)))
        .collect();

    let report = render(&reports);
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sst-musashi-report.md");
    fs::write(&output, &report).expect("writing the report");
    println!("{report}\nreport written to {}", output.display());
}

fn suite_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/m68000/v1")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn run_file(core: &mut Musashi, path: &Path) -> FileReport {
    let cases = sst::load_file(path).unwrap_or_else(|error| panic!("{error}"));
    let mut report = FileReport {
        cases: cases.len(),
        ..FileReport::default()
    };
    for case in &cases {
        let cycles = run_case(core, case);
        let fields = mismatched_fields(core, case);
        if raises_address_error(case) {
            report.address_errors += 1;
            report.address_errors_passed += usize::from(fields.is_empty());
            continue;
        }
        if fields.is_empty() {
            report.passed += 1;
        } else if report.sample_failures.len() < SAMPLE_FAILURES {
            report
                .sample_failures
                .push(format!("{} ({})", case.name, fields.join(", ")));
        }
        for field in fields {
            *report.mismatches.entry(field).or_default() += 1;
        }
        report.write_matches += usize::from(write_order_matches(core, case));
        report.cycle_matches += usize::from(u32::try_from(cycles) == Ok(case.cycles));
    }
    report
}

fn raises_address_error(case: &TestCase) -> bool {
    case.transactions.iter().any(|transaction| {
        matches!(
            transaction,
            Transaction::Access {
                kind: TransactionKind::ReadAddressError | TransactionKind::WriteAddressError,
                ..
            }
        )
    })
}

/// Loads the initial state, executes one instruction and returns Musashi's cycle count.
fn run_case(core: &mut Musashi, case: &TestCase) -> i32 {
    core.clear_memory();
    for &(address, value) in &case.initial.ram {
        core.write_byte(address, value);
    }
    // Musashi does not model the prefetch queue, so the queued words go back into memory
    // in front of the prefetch address and Musashi refetches them.
    let opcode_address = case.initial.registers.pc.wrapping_sub(PC_OFFSET);
    for (offset, word) in [0, 2].into_iter().zip(case.initial.prefetch) {
        let [_, _, high, low] = word.to_be_bytes();
        core.write_byte(opcode_address + offset, high);
        core.write_byte(opcode_address + offset + 1, low);
    }
    core.set_registers(&Registers {
        pc: opcode_address,
        ..case.initial.registers
    });
    core.step()
}

fn mismatched_fields(core: &Musashi, case: &TestCase) -> Vec<&'static str> {
    const DATA: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
    const ADDRESS: [&str; 7] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6"];

    let actual = core.registers();
    let expected = &case.expected.registers;
    let mut fields = Vec::new();
    for (index, name) in DATA.into_iter().enumerate() {
        if actual.d[index] != expected.d[index] {
            fields.push(name);
        }
    }
    for (index, name) in ADDRESS.into_iter().enumerate() {
        if actual.a[index] != expected.a[index] {
            fields.push(name);
        }
    }
    if actual.usp != expected.usp {
        fields.push("USP");
    }
    if actual.ssp != expected.ssp {
        fields.push("SSP");
    }
    let pc_offset = if core.is_stopped() { 0 } else { PC_OFFSET };
    if actual.pc.wrapping_add(pc_offset) != expected.pc {
        fields.push("PC");
    }
    for (bit, name) in SR_FLAGS {
        if (actual.sr ^ expected.sr) & bit != 0 {
            fields.push(name);
        }
    }
    if (actual.sr ^ expected.sr) & 0xFF00 != 0 {
        fields.push("SR.system");
    }
    if case
        .expected
        .ram
        .iter()
        .any(|&(address, value)| core.read_byte(address) != value)
    {
        fields.push("RAM");
    }
    fields
}

/// Compares the order of write cycles by word address. Reads are left out because the real
/// 68000 fetches two words ahead and Musashi does not, so every instruction-stream read
/// lands at a different address. Data values are left out because SingleStepTests records
/// the raw data bus, with byte writes duplicated across both lanes.
fn write_order_matches(core: &Musashi, case: &TestCase) -> bool {
    let expected = case
        .transactions
        .iter()
        .filter_map(|transaction| match transaction {
            Transaction::Access {
                kind: TransactionKind::Write,
                address,
                ..
            } => Some(address & 0x00FF_FFFE),
            _ => None,
        });
    let actual = core
        .bus_log()
        .iter()
        .filter(|access| access.kind == AccessKind::Write)
        .map(|access| access.address & 0x00FF_FFFE);
    expected.eq(actual)
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / whole as f64
}

/// Columns after "Cases" cover the cases without an address error, except "AE pass".
fn render(reports: &[(String, FileReport)]) -> String {
    let mut out = String::from(
        "| File | Cases | No AE | Pass | Pass % | AE pass | Write order % | Cycles % | \
         Top mismatches |\n\
         |------|------:|------:|-----:|-------:|--------:|--------------:|---------:|\
         ----------------|\n",
    );
    let mut total = FileReport::default();
    for (name, report) in reports {
        let marker = if UNRELIABLE.contains(&name.as_str()) {
            " (unreliable upstream)"
        } else {
            ""
        };
        render_row(&mut out, &format!("{name}{marker}"), report);
        total.add(report);
    }
    render_row(&mut out, "**Total**", &total);

    out.push_str("\nSample failures:\n\n");
    for (name, report) in reports {
        for failure in &report.sample_failures {
            let _ = writeln!(out, "- {name}: {failure}");
        }
    }
    out
}

fn render_row(out: &mut String, name: &str, report: &FileReport) {
    let checked = report.cases - report.address_errors;
    let mut top: Vec<_> = report.mismatches.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    let top = top
        .iter()
        .take(4)
        .map(|(field, count)| format!("{field} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "| {name} | {} | {checked} | {} | {:.2} | {}/{} | {:.1} | {:.1} | {top} |",
        report.cases,
        report.passed,
        percent(report.passed, checked),
        report.address_errors_passed,
        report.address_errors,
        percent(report.write_matches, checked),
        percent(report.cycle_matches, checked),
    );
}
