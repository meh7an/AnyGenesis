//! Runs the whole SingleStepTests/m68000 suite through decode, lift and the IR interpreter.
//!
//! A case passes when D0-D7, A0-A6, USP, SSP, SR, PC (its 24 address bits), RAM and the
//! cycle count match. Cases that raise an address error are excluded (the CPU fidelity bar
//! does not model them); every other case must pass. A cycle count may differ only where
//! [`CYCLE_EXCEPTIONS`] lists it, each signed off by Mehran. Bus transactions and prefetch are
//! not compared.
//!
//! Needs `cargo xtask fetch-sst`. Run with
//! `cargo test -p anygenesis-ir --test sst -- --ignored --nocapture`; the report lands in
//! target/tmp/sst-ir-report.md.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anygenesis_ir::interp::{Cpu, Interpreter, Step, Transfer};
use anygenesis_oracle::sst::{self, TestCase, Transaction, TransactionKind};
use anygenesis_shared::{Address, Bus};

/// SingleStepTests stores PC as MAME's prefetch address, this far past the next opcode. A
/// stopped CPU has not prefetched, so its stored PC is the real one.
const PC_OFFSET: u32 = 4;

/// The PC is compared on the 24 address bits. The IR's addresses are 24-bit, so the upper
/// byte a computed jump, return or vector leaves in the real PC is not modelled (Mehran,
/// M2.2); the report counts the cases where it differs.
const PC_BITS: u32 = 0x00FF_FFFF;

const SR_FLAGS: [(u16, &str); 5] = [
    (0x10, "SR.X"),
    (0x08, "SR.N"),
    (0x04, "SR.Z"),
    (0x02, "SR.V"),
    (0x01, "SR.C"),
];

/// Failing case names kept per file for the report.
const SAMPLE_FAILURES: usize = 3;

/// Files listed in the report's largest-cycle-error section.
const WORST_CYCLE_FILES: usize = 10;

/// Cycle mismatches kept per file for the report.
const SAMPLE_CYCLE_MISMATCHES: usize = 12;

/// Cases whose cycle count may differ from the suite's, each signed off by Mehran.
struct CycleException {
    /// The suite file the cases come from.
    file: &'static str,
    /// Whether a case of that file is covered.
    covers: fn(&TestCase) -> bool,
    reason: &'static str,
}

const CYCLE_EXCEPTIONS: &[CycleException] = &[];

#[derive(Debug, Default)]
struct FileReport {
    cases: usize,
    address_errors: usize,
    passed: usize,
    /// Checked cases whose expected PC has bits above the 24 the IR models.
    pc_upper_byte: usize,
    cycle_exact: usize,
    /// Checked cases a [`CycleException`] covers, and those of them whose count differs.
    cycle_excepted: usize,
    cycle_excepted_differing: usize,
    /// Sums over checked cases of |ours - actual| and of ours - actual.
    cycle_absolute_error: u64,
    cycle_signed_error: i64,
    /// Mismatch counts per state field.
    mismatches: BTreeMap<&'static str, usize>,
    sample_failures: Vec<String>,
    sample_cycle_mismatches: Vec<String>,
}

impl FileReport {
    fn checked(&self) -> usize {
        self.cases - self.address_errors
    }

    fn add(&mut self, other: &Self) {
        self.cases += other.cases;
        self.address_errors += other.address_errors;
        self.passed += other.passed;
        self.pc_upper_byte += other.pc_upper_byte;
        self.cycle_exact += other.cycle_exact;
        self.cycle_excepted += other.cycle_excepted;
        self.cycle_excepted_differing += other.cycle_excepted_differing;
        self.cycle_absolute_error += other.cycle_absolute_error;
        self.cycle_signed_error += other.cycle_signed_error;
    }

    fn mean_absolute_error(&self) -> f64 {
        ratio(self.cycle_absolute_error as f64, self.checked())
    }

    fn mean_signed_error(&self) -> f64 {
        ratio(self.cycle_signed_error as f64, self.checked())
    }
}

#[test]
#[ignore = "needs testdata/m68000 from `cargo xtask fetch-sst`"]
fn test_sst_suite_through_ir_interpreter_passes() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/m68000/v1");
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

    let mut interpreter = Interpreter::default();
    let reports: Vec<(String, FileReport)> = files
        .iter()
        .map(|path| {
            let name = file_name(path);
            let report = run_file(&mut interpreter, path, &name);
            (name, report)
        })
        .collect();

    let report = render(&reports);
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sst-ir-report.md");
    fs::write(&output, &report).expect("writing the report");
    println!("{report}\nreport written to {}", output.display());

    let failed: usize = reports
        .iter()
        .map(|(_, report)| report.checked() - report.passed)
        .sum();
    assert_eq!(failed, 0, "{failed} cases failed; see {}", output.display());
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A sparse 24-bit memory; bytes never written read as 0.
#[derive(Debug, Default)]
struct Memory(HashMap<u32, u8>);

impl Bus for Memory {
    fn read_byte(&mut self, address: Address) -> u8 {
        self.0.get(&address.get()).copied().unwrap_or(0)
    }

    fn read_word(&mut self, address: Address) -> u16 {
        let high = self.read_byte(address);
        let low = self.read_byte(address.wrapping_add(1));
        u16::from_be_bytes([high, low])
    }

    fn write_byte(&mut self, address: Address, value: u8) {
        self.0.insert(address.get(), value);
    }

    fn write_word(&mut self, address: Address, value: u16) {
        let [high, low] = value.to_be_bytes();
        self.write_byte(address, high);
        self.write_byte(address.wrapping_add(1), low);
    }
}

fn run_file(interpreter: &mut Interpreter, path: &Path, name: &str) -> FileReport {
    let cases = sst::load_file(path).unwrap_or_else(|error| panic!("{error}"));
    let exceptions: Vec<&CycleException> = CYCLE_EXCEPTIONS
        .iter()
        .filter(|exception| exception.file == name)
        .collect();
    let mut report = FileReport {
        cases: cases.len(),
        ..FileReport::default()
    };
    for case in &cases {
        if raises_address_error(case) {
            report.address_errors += 1;
            continue;
        }
        let (cpu, mut memory, step) = run_case(interpreter, case);
        report.pc_upper_byte += usize::from(case.expected.registers.pc & !PC_BITS != 0);
        let mut fields = mismatched_fields(&cpu, &mut memory, &step, case);
        let excepted = exceptions.iter().any(|exception| (exception.covers)(case));
        let cycles_differ = u32::from(step.cycles) != case.cycles;
        report.cycle_excepted += usize::from(excepted);
        report.cycle_excepted_differing += usize::from(excepted && cycles_differ);
        if cycles_differ && !excepted {
            fields.push("cycles");
            if report.sample_cycle_mismatches.len() < SAMPLE_CYCLE_MISMATCHES {
                report.sample_cycle_mismatches.push(format!(
                    "{}: ours {}, suite {}",
                    case.name, step.cycles, case.cycles
                ));
            }
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
        let error = i64::from(step.cycles) - i64::from(case.cycles);
        report.cycle_exact += usize::from(error == 0);
        report.cycle_absolute_error += error.unsigned_abs();
        report.cycle_signed_error += error;
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

/// Loads the initial state and executes one instruction.
fn run_case(interpreter: &mut Interpreter, case: &TestCase) -> (Cpu, Memory, Step) {
    let initial = &case.initial.registers;
    let mut memory = Memory::default();
    for &(address, value) in &case.initial.ram {
        memory.write_byte(Address::new(address), value);
    }
    // The interpreter has no prefetch queue: the queued words go back into memory in front
    // of the prefetch address, where the opcode was fetched from.
    let opcode_address = Address::new(initial.pc.wrapping_sub(PC_OFFSET));
    for (offset, word) in [0, 2].into_iter().zip(case.initial.prefetch) {
        // A queue entry holds one word.
        memory.write_word(opcode_address.wrapping_add(offset), word as u16);
    }
    // The suite's SR holds only the bits the 68000 has, so it fits a u16.
    let mut cpu = Cpu::new(initial.sr as u16, initial.usp, initial.ssp);
    cpu.d = initial.d;
    cpu.a[..7].copy_from_slice(&initial.a);
    cpu.pc = opcode_address;
    let step = interpreter.step(&mut cpu, &mut memory);
    (cpu, memory, step)
}

fn mismatched_fields(
    cpu: &Cpu,
    memory: &mut Memory,
    step: &Step,
    case: &TestCase,
) -> Vec<&'static str> {
    const DATA: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
    const ADDRESS: [&str; 7] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6"];

    let expected = &case.expected.registers;
    let mut fields = Vec::new();
    for (index, name) in DATA.into_iter().enumerate() {
        if cpu.d[index] != expected.d[index] {
            fields.push(name);
        }
    }
    for (index, name) in ADDRESS.into_iter().enumerate() {
        if cpu.a[index] != expected.a[index] {
            fields.push(name);
        }
    }
    if cpu.usp() != expected.usp {
        fields.push("USP");
    }
    if cpu.ssp() != expected.ssp {
        fields.push("SSP");
    }
    let pc_offset = match step.outcome.transfer {
        Some(Transfer::Stop { .. }) => 0,
        _ => PC_OFFSET,
    };
    if cpu.pc.wrapping_add(pc_offset).get() != expected.pc & PC_BITS {
        fields.push("PC");
    }
    let sr = u32::from(cpu.sr());
    for (bit, name) in SR_FLAGS {
        if (sr ^ expected.sr) & u32::from(bit) != 0 {
            fields.push(name);
        }
    }
    if (sr ^ expected.sr) & 0xFF00 != 0 {
        fields.push("SR.system");
    }
    if case
        .expected
        .ram
        .iter()
        .any(|&(address, value)| memory.read_byte(Address::new(address)) != value)
    {
        fields.push("RAM");
    }
    fields
}

fn ratio(part: f64, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part / whole as f64
}

fn render(reports: &[(String, FileReport)]) -> String {
    let mut out = String::from(
        "| File | Cases | AE excluded | Checked | Pass | Pass % | Cycles exact % | \
         Mean abs cycle error | Mean signed cycle error | Top mismatches |\n\
         |------|------:|------------:|--------:|-----:|-------:|---------------:|\
         ---------------------:|------------------------:|----------------|\n",
    );
    let mut total = FileReport::default();
    for (name, report) in reports {
        render_row(&mut out, name, report);
        total.add(report);
    }
    render_row(&mut out, "**Total**", &total);
    let _ = writeln!(
        out,
        "\nPC is compared on its 24 address bits: in {} checked cases the expected PC has a \
         nonzero upper byte, which the IR does not model.",
        total.pc_upper_byte
    );

    let mut worst: Vec<_> = reports.iter().collect();
    worst.sort_by(|a, b| {
        b.1.mean_absolute_error()
            .total_cmp(&a.1.mean_absolute_error())
    });
    out.push_str("\nLargest mean absolute cycle error:\n\n");
    for (name, report) in worst.into_iter().take(WORST_CYCLE_FILES) {
        let _ = writeln!(
            out,
            "- {name}: {:.1} (signed {:+.1}, exact {:.1}%)",
            report.mean_absolute_error(),
            report.mean_signed_error(),
            100.0 * ratio(report.cycle_exact as f64, report.checked()),
        );
    }

    let _ = writeln!(
        out,
        "\nCycle exceptions: {} checked cases covered, {} of them differing.",
        total.cycle_excepted, total.cycle_excepted_differing
    );
    for exception in CYCLE_EXCEPTIONS {
        let _ = writeln!(out, "- {}: {}", exception.file, exception.reason);
    }

    out.push_str("\nSample failures:\n\n");
    for (name, report) in reports {
        for failure in &report.sample_failures {
            let _ = writeln!(out, "- {name}: {failure}");
        }
    }

    out.push_str("\nSample cycle mismatches:\n\n");
    for (name, report) in reports {
        for mismatch in &report.sample_cycle_mismatches {
            let _ = writeln!(out, "- {name}: {mismatch}");
        }
    }
    out
}

fn render_row(out: &mut String, name: &str, report: &FileReport) {
    let checked = report.checked();
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
        "| {name} | {} | {} | {checked} | {} | {:.2} | {:.1} | {:.1} | {:+.1} | {top} |",
        report.cases,
        report.address_errors,
        report.passed,
        100.0 * ratio(report.passed as f64, checked),
        100.0 * ratio(report.cycle_exact as f64, checked),
        report.mean_absolute_error(),
        report.mean_signed_error(),
    );
}
