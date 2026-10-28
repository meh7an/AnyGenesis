//! The translated SingleStepTests sample: each case's one-instruction function and its
//! stepped function (M2.5) run from the case's initial state, and D0-D7, A0-A6, USP, SSP,
//! SR, PC (24 bits) and RAM must match the expected state, as in M2.2, and the cycles the
//! code charged the suite's count (M1.1). The interpreter passes the whole suite (the ir
//! crate's test), so stepped code equals it on every case here.
//!
//! Needs `cargo xtask fetch-sst` before the crate is built (the build script translates the
//! sample). Run with `cargo test -p anygenesis-c-oracle --test sst_c -- --ignored
//! --nocapture`; the report lands in target/tmp/sst-c-report.md.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    fs,
    path::Path,
};

use anygenesis_abi::{AgFn, AgState};
use anygenesis_c_oracle::{
    bus::{Device, RAM_START, Sparse},
    host::{self, Host, POISON},
    programs::{self, SstCase},
};
use anygenesis_oracle::sst::{self, TestCase};
use anygenesis_shared::Address;

const PC_OFFSET: u32 = 4;
const PC_BITS: u32 = Address::MASK;
const SUPERVISOR: u32 = 0x2000;
const RAM_MASK: u32 = 0xFFFF;
const SR_FLAGS: [(u32, &str); 5] = [
    (0x10, "SR.X"),
    (0x08, "SR.N"),
    (0x04, "SR.Z"),
    (0x02, "SR.V"),
    (0x01, "SR.C"),
];
const SAMPLE_FAILURES: usize = 3;
/// Cases of the negative control, spread over the sample.
const CONTROL_CASES: usize = 254;

/// The suite's flat memory; `ag_lookup` records the exit and ends the call.
struct SstHost {
    memory: Sparse,
    exit: Option<Address>,
    stopped: bool,
}

impl Host for SstHost {
    fn memory(&mut self) -> (&[u8], &mut dyn Device) {
        (&[], &mut self.memory)
    }

    fn lookup(&mut self, address: Address) -> AgFn {
        self.exit.get_or_insert(address);
        host::poisoned
    }

    fn stop(&mut self, resume: Address) -> u32 {
        self.stopped = true;
        resume.get()
    }

    fn check(&mut self, _state: &mut AgState, pc: Address) -> u32 {
        pc.get()
    }

    fn reset_devices(&mut self) {}
}

#[derive(Debug, Default)]
struct FileReport {
    translated: usize,
    passed: usize,
    mismatches: BTreeMap<&'static str, usize>,
    sample_failures: Vec<String>,
}

#[test]
#[ignore = "needs testdata/m68000 from `cargo xtask fetch-sst` before the build"]
fn test_sst_sample_through_translated_c_passes() {
    let Some(sample) = programs::SST_SAMPLE else {
        panic!("the suite was missing when the crate was built: fetch it and rebuild");
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/m68000/v1");
    let mut by_file: BTreeMap<&str, Vec<&SstCase>> = BTreeMap::new();
    for case in programs::SST_CASES {
        by_file.entry(case.file).or_default().push(case);
    }
    let mut reports: Vec<(&str, FileReport)> = Vec::new();
    for (file, cases) in by_file {
        let suite = sst::load_file(&directory.join(file)).unwrap_or_else(|error| panic!("{error}"));
        let mut report = FileReport::default();
        for case in cases {
            let test = &suite[case.index];
            for (form, function) in [("function", case.function), ("stepped", case.stepped)] {
                let fields = run_case(test, function);
                report.translated += 1;
                if fields.is_empty() {
                    report.passed += 1;
                } else if report.sample_failures.len() < SAMPLE_FAILURES {
                    let text = format!("{} {form} ({})", test.name, fields.join(", "));
                    report.sample_failures.push(text);
                }
                for field in fields {
                    *report.mismatches.entry(field).or_default() += 1;
                }
            }
        }
        reports.push((file, report));
    }

    // Guards the comparison: run cases with their neighbour's function, nearly all must fail.
    let control: Vec<&SstCase> = programs::SST_CASES
        .iter()
        .step_by(programs::SST_CASES.len() / CONTROL_CASES)
        .collect();
    let mut suites: HashMap<&str, Vec<TestCase>> = HashMap::new();
    let mut caught = 0;
    for pair in control.windows(2) {
        let suite = suites.entry(pair[0].file).or_insert_with(|| {
            sst::load_file(&directory.join(pair[0].file)).unwrap_or_else(|error| panic!("{error}"))
        });
        caught += usize::from(!run_case(&suite[pair[0].index], pair[1].function).is_empty());
    }
    assert!(
        caught + CONTROL_CASES / 10 >= control.len() - 1,
        "only {caught} of {} swapped cases failed",
        control.len() - 1
    );

    let report = render(sample, &reports);
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sst-c-report.md");
    fs::write(&output, &report).expect("writing the report");
    println!("{report}\nreport written to {}", output.display());
    let failed: usize = reports
        .iter()
        .map(|(_, report)| report.translated - report.passed)
        .sum();
    assert_eq!(failed, 0, "{failed} cases failed; see {}", output.display());
}

/// Runs a case's translated instruction; returns the fields that differ.
fn run_case(case: &TestCase, function: AgFn) -> Vec<&'static str> {
    let initial = &case.initial.registers;
    let address = Address::new(initial.pc.wrapping_sub(PC_OFFSET));
    let mut memory = Sparse::default();
    for &(at, value) in &case.initial.ram {
        memory.set(at, value);
    }
    for (offset, word) in [0, 2].into_iter().zip(case.initial.prefetch) {
        // A queue entry holds one word.
        let [high, low] = (word as u16).to_be_bytes();
        memory.set(address.wrapping_add(offset).get(), high);
        memory.set(address.wrapping_add(offset + 1).get(), low);
    }
    let mut state = host::new_state();
    state.d = initial.d;
    state.a[..7].copy_from_slice(&initial.a);
    let supervisor = initial.sr & SUPERVISOR != 0;
    let (active, inactive) = if supervisor {
        (initial.ssp, initial.usp)
    } else {
        (initial.usp, initial.ssp)
    };
    state.a[7] = active;
    state.inactive_sp = inactive;
    // The suite's SR holds only the bits the 68000 has.
    state.sr = (initial.sr & 0xFF00) as u16;
    let flag = |bit: u32| u8::from(initial.sr & bit != 0);
    (state.x, state.n, state.z, state.v, state.c) =
        (flag(0x10), flag(0x08), flag(0x04), flag(0x02), flag(0x01));
    for (&at, &value) in &memory.0 {
        if at >= RAM_START {
            state.ram[(at & RAM_MASK) as usize] = value;
        }
    }
    let mut host = SstHost {
        memory,
        exit: None,
        stopped: false,
    };
    let next = host::call(&mut state, &mut host, function, address);
    let pc = if next & POISON != 0 {
        host.exit.map_or(next, Address::get)
    } else {
        next
    };
    mismatched_fields(case, &state, &host, pc)
}

fn mismatched_fields(
    case: &TestCase,
    state: &AgState,
    host: &SstHost,
    pc: u32,
) -> Vec<&'static str> {
    const DATA: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
    const ADDRESS: [&str; 7] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6"];

    let expected = &case.expected.registers;
    let mut fields = Vec::new();
    for (index, name) in DATA.into_iter().enumerate() {
        if state.d[index] != expected.d[index] {
            fields.push(name);
        }
    }
    for (index, name) in ADDRESS.into_iter().enumerate() {
        if state.a[index] != expected.a[index] {
            fields.push(name);
        }
    }
    let (usp, ssp) = if u32::from(state.sr) & SUPERVISOR != 0 {
        (state.inactive_sp, state.a[7])
    } else {
        (state.a[7], state.inactive_sp)
    };
    if usp != expected.usp {
        fields.push("USP");
    }
    if ssp != expected.ssp {
        fields.push("SSP");
    }
    let offset = if host.stopped { 0 } else { PC_OFFSET };
    if pc.wrapping_add(offset) & PC_BITS != expected.pc & PC_BITS {
        fields.push("PC");
    }
    let sr = u32::from(state.sr)
        | u32::from(state.x) << 4
        | u32::from(state.n) << 3
        | u32::from(state.z) << 2
        | u32::from(state.v) << 1
        | u32::from(state.c);
    for (bit, name) in SR_FLAGS {
        if (sr ^ expected.sr) & bit != 0 {
            fields.push(name);
        }
    }
    if (sr ^ expected.sr) & 0xFF00 != 0 {
        fields.push("SR.system");
    }
    let byte = |at: u32| {
        if at >= RAM_START {
            state.ram[(at & RAM_MASK) as usize]
        } else {
            host.memory.byte(at)
        }
    };
    if case
        .expected
        .ram
        .iter()
        .any(|&(at, value)| byte(at & PC_BITS) != value)
    {
        fields.push("RAM");
    }
    // The count started at 0 and only went down.
    if i64::from(state.cycles) != -i64::from(case.cycles) {
        fields.push("cycles");
    }
    fields
}

fn render(sample: usize, reports: &[(&str, FileReport)]) -> String {
    let mut excluded: HashMap<&str, Vec<String>> = HashMap::new();
    for exclusion in programs::SST_EXCLUDED {
        excluded
            .entry(exclusion.file)
            .or_default()
            .push(format!("{} {}", exclusion.reason, exclusion.count));
    }
    let mut out = format!(
        "Sample: up to {sample} cases per file, distinct opcode words first, each run as its \
         function and as its stepped function.\n\n\
         | File | Translated | Pass | Pass % | Left out | Top mismatches |\n\
         |------|-----------:|-----:|-------:|----------|----------------|\n"
    );
    let (mut translated, mut passed) = (0, 0);
    for (name, report) in reports {
        translated += report.translated;
        passed += report.passed;
        let top = report
            .mismatches
            .iter()
            .map(|(field, count)| format!("{field} {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let left_out = excluded
            .get(name)
            .map(|items| items.join(", "))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {:.2} | {left_out} | {top} |",
            report.translated,
            report.passed,
            100.0 * report.passed as f64 / report.translated.max(1) as f64,
        );
    }
    let _ = writeln!(
        out,
        "| **Total** | {translated} | {passed} | {:.2} | | |",
        100.0 * passed as f64 / translated.max(1) as f64
    );
    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for exclusion in programs::SST_EXCLUDED {
        *totals.entry(exclusion.reason).or_default() += exclusion.count;
    }
    out.push_str("\nLeft out of the sample, all files:\n\n");
    for (reason, count) in totals {
        let _ = writeln!(out, "- {reason}: {count}");
    }
    out.push_str("\nSample failures:\n\n");
    for (name, report) in reports {
        for failure in &report.sample_failures {
            let _ = writeln!(out, "- {name}: {failure}");
        }
    }
    out
}
