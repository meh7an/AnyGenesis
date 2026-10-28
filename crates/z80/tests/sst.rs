//! Runs the whole SingleStepTests/z80 suite through the interpreter.
//!
//! A case passes when every register and the internal state the suite records (WZ, Q, P,
//! EI, IM, IFF1, IFF2) match, RAM matches, the port accesses match in order, the T-state
//! count matches, and every memory and I/O access happens at the suite's T-state with its
//! address and data. Every case must pass except those [`EXCLUSIONS`] lists, each signed off
//! by Mehran.
//!
//! Needs `cargo xtask fetch-sst z80`. Run with
//! `cargo test --release -p anygenesis-z80 --test sst -- --ignored --nocapture`; the report
//! lands in target/tmp/sst-z80-report.md.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anygenesis_oracle::sst_z80::{self, Port, Registers, TestCase};
use anygenesis_z80::{Bus, Z80};

/// Failing case names kept per file for the report.
const SAMPLE_FAILURES: usize = 3;

/// Cases left out of the pass count, each signed off by Mehran.
struct Exclusion {
    /// The suite file the cases come from.
    file: &'static str,
    /// Whether a case of that file is covered.
    covers: fn(&TestCase) -> bool,
    reason: &'static str,
}

const EXCLUSIONS: &[Exclusion] = &[];

const F_FLAGS: [(u8, &str); 8] = [
    (0x80, "F.S"),
    (0x40, "F.Z"),
    (0x20, "F.Y"),
    (0x10, "F.H"),
    (0x08, "F.X"),
    (0x04, "F.PV"),
    (0x02, "F.N"),
    (0x01, "F.C"),
];

#[derive(Debug, Default)]
struct FileReport {
    cases: usize,
    excluded: usize,
    passed: usize,
    /// Mismatch counts per state field.
    mismatches: BTreeMap<&'static str, usize>,
    sample_failures: Vec<String>,
}

impl FileReport {
    fn checked(&self) -> usize {
        self.cases - self.excluded
    }

    fn add(&mut self, other: &Self) {
        self.cases += other.cases;
        self.excluded += other.excluded;
        self.passed += other.passed;
        for (field, count) in &other.mismatches {
            *self.mismatches.entry(field).or_default() += count;
        }
    }
}

#[test]
#[ignore = "needs testdata/z80 from `cargo xtask fetch-sst z80`"]
fn test_sst_suite_through_interpreter_passes() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/z80/v1");
    let mut files: Vec<PathBuf> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()))
        .map(|entry| entry.expect("listing the suite directory").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no .json files in {}",
        directory.display()
    );

    let reports: Vec<(String, FileReport)> = files
        .iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let report = run_file(path, &name);
            (name, report)
        })
        .collect();

    let report = render(&reports);
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sst-z80-report.md");
    fs::write(&output, &report).expect("writing the report");
    println!("{report}\nreport written to {}", output.display());

    let failed: usize = reports
        .iter()
        .map(|(_, report)| report.checked() - report.passed)
        .sum();
    assert_eq!(failed, 0, "{failed} cases failed; see {}", output.display());
}

/// A memory or I/O access as the suite's cycle log shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Access {
    /// The T-state of the read or write strobe.
    at: u32,
    io: bool,
    write: bool,
    address: u16,
    /// `None` where the log leaves the data bus floating.
    value: Option<u8>,
}

/// Memory holding only the case's bytes, and the case's port reads in order.
#[derive(Debug)]
struct TestBus<'a> {
    ram: Vec<(u16, u8)>,
    ports: &'a [Port],
    accesses: Vec<Access>,
    port_log: Vec<Port>,
}

impl TestBus<'_> {
    fn byte(&self, address: u16) -> u8 {
        self.ram
            .iter()
            .find(|&&(at, _)| at == address)
            .map_or(0, |&(_, value)| value)
    }

    fn record(&mut self, at: u32, io: bool, write: bool, address: u16, value: u8) {
        self.accesses.push(Access {
            at,
            io,
            write,
            address,
            value: Some(value),
        });
    }
}

impl Bus for TestBus<'_> {
    fn read(&mut self, address: u16, at: u32) -> u8 {
        let value = self.byte(address);
        self.record(at, false, false, address, value);
        value
    }

    fn write(&mut self, address: u16, value: u8, at: u32) {
        match self.ram.iter_mut().find(|(at, _)| *at == address) {
            Some((_, byte)) => *byte = value,
            None => self.ram.push((address, value)),
        }
        self.record(at, false, true, address, value);
    }

    fn input(&mut self, port: u16, at: u32) -> u8 {
        let value = self
            .ports
            .get(self.port_log.len())
            .filter(|expected| !expected.write && expected.address == port)
            .map_or(0xFF, |expected| expected.value);
        self.port_log.push(Port {
            address: port,
            value,
            write: false,
        });
        self.record(at, true, false, port, value);
        value
    }

    fn output(&mut self, port: u16, value: u8, at: u32) {
        self.port_log.push(Port {
            address: port,
            value,
            write: true,
        });
        self.record(at, true, true, port, value);
    }
}

fn run_file(path: &Path, name: &str) -> FileReport {
    let cases = sst_z80::load_file(path).unwrap_or_else(|error| panic!("{name}: {error}"));
    let exclusions: Vec<&Exclusion> = EXCLUSIONS
        .iter()
        .filter(|exclusion| exclusion.file == name)
        .collect();
    let mut report = FileReport {
        cases: cases.len(),
        ..FileReport::default()
    };
    for case in &cases {
        if exclusions.iter().any(|exclusion| (exclusion.covers)(case)) {
            report.excluded += 1;
            continue;
        }
        let fields = mismatched_fields(case);
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
    }
    report
}

fn cpu_from(registers: &Registers) -> Z80 {
    let mut cpu = Z80::default();
    let Registers {
        pc,
        sp,
        a,
        f,
        b,
        c,
        d,
        e,
        h,
        l,
        af_,
        bc_,
        de_,
        hl_,
        ix,
        iy,
        i,
        r,
        wz,
        im,
        iff1,
        iff2,
        ei,
        p,
        q,
    } = *registers;
    (cpu.pc, cpu.sp, cpu.ix, cpu.iy, cpu.wz) = (pc, sp, ix, iy, wz);
    (cpu.a, cpu.f, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l) = (a, f, b, c, d, e, h, l);
    (cpu.af_, cpu.bc_, cpu.de_, cpu.hl_) = (af_, bc_, de_, hl_);
    (cpu.i, cpu.r, cpu.im) = (i, r, im);
    (cpu.iff1, cpu.iff2, cpu.ei, cpu.p, cpu.q) = (iff1, iff2, ei, p, q);
    cpu
}

/// The accesses the cycle log shows: a read's data is on the bus one T-state after its
/// strobe, a write's during it.
fn expected_accesses(case: &TestCase) -> Vec<Access> {
    let cycles = &case.cycles;
    cycles
        .iter()
        .enumerate()
        .filter(|(_, cycle)| cycle.read || cycle.write)
        .map(|(at, cycle)| Access {
            at: at as u32,
            io: cycle.io_request,
            write: cycle.write,
            address: cycle.address.unwrap_or_default(),
            value: if cycle.write {
                cycle.data
            } else {
                cycles.get(at + 1).and_then(|next| next.data)
            },
        })
        .collect()
}

fn accesses_match(ours: &[Access], expected: &[Access]) -> bool {
    ours.len() == expected.len()
        && ours.iter().zip(expected).all(|(ours, expected)| {
            Access {
                value: expected.value.and(ours.value),
                ..*ours
            } == *expected
        })
}

fn mismatched_fields(case: &TestCase) -> Vec<&'static str> {
    let mut cpu = cpu_from(&case.initial.registers);
    let mut bus = TestBus {
        ram: case.initial.ram.clone(),
        ports: &case.ports,
        accesses: Vec::new(),
        port_log: Vec::new(),
    };
    let t_states = cpu.step(&mut bus);

    let expected = &case.expected.registers;
    let ours = Registers {
        pc: cpu.pc,
        sp: cpu.sp,
        a: cpu.a,
        f: cpu.f,
        b: cpu.b,
        c: cpu.c,
        d: cpu.d,
        e: cpu.e,
        h: cpu.h,
        l: cpu.l,
        af_: cpu.af_,
        bc_: cpu.bc_,
        de_: cpu.de_,
        hl_: cpu.hl_,
        ix: cpu.ix,
        iy: cpu.iy,
        i: cpu.i,
        r: cpu.r,
        wz: cpu.wz,
        im: cpu.im,
        iff1: cpu.iff1,
        iff2: cpu.iff2,
        ei: cpu.ei,
        p: cpu.p,
        q: cpu.q,
    };
    let checks: [(&str, bool); 24] = [
        ("PC", ours.pc == expected.pc),
        ("SP", ours.sp == expected.sp),
        ("A", ours.a == expected.a),
        ("B", ours.b == expected.b),
        ("C", ours.c == expected.c),
        ("D", ours.d == expected.d),
        ("E", ours.e == expected.e),
        ("H", ours.h == expected.h),
        ("L", ours.l == expected.l),
        ("AF'", ours.af_ == expected.af_),
        ("BC'", ours.bc_ == expected.bc_),
        ("DE'", ours.de_ == expected.de_),
        ("HL'", ours.hl_ == expected.hl_),
        ("IX", ours.ix == expected.ix),
        ("IY", ours.iy == expected.iy),
        ("I", ours.i == expected.i),
        ("R", ours.r == expected.r),
        ("WZ", ours.wz == expected.wz),
        ("IM", ours.im == expected.im),
        ("IFF1", ours.iff1 == expected.iff1),
        ("IFF2", ours.iff2 == expected.iff2),
        ("EI", ours.ei == expected.ei),
        ("P", ours.p == expected.p),
        ("Q", ours.q == expected.q),
    ];
    let mut fields: Vec<&str> = checks
        .into_iter()
        .filter(|(_, matches)| !matches)
        .map(|(name, _)| name)
        .collect();
    for (bit, name) in F_FLAGS {
        if (ours.f ^ expected.f) & bit != 0 {
            fields.push(name);
        }
    }
    if case
        .expected
        .ram
        .iter()
        .any(|&(address, value)| bus.byte(address) != value)
    {
        fields.push("RAM");
    }
    if bus.port_log != case.ports {
        fields.push("ports");
    }
    if t_states as usize != case.cycles.len() {
        fields.push("T-states");
    }
    if !accesses_match(&bus.accesses, &expected_accesses(case)) {
        fields.push("accesses");
    }
    fields
}

fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 / whole as f64
}

fn render(reports: &[(String, FileReport)]) -> String {
    let mut out = String::from(
        "| File | Cases | Excluded | Checked | Pass | Pass % | Top mismatches |\n\
         |------|------:|---------:|--------:|-----:|-------:|----------------|\n",
    );
    let mut total = FileReport::default();
    for (name, report) in reports {
        if report.passed != report.checked() || report.excluded != 0 {
            render_row(&mut out, name, report);
        }
        total.add(report);
    }
    render_row(&mut out, "**Total**", &total);
    let _ = writeln!(
        out,
        "\nFiles where every checked case passes and none is excluded are left out of the \
         table ({} files in all).",
        reports.len()
    );

    let _ = writeln!(out, "\nExclusions: {} cases.", total.excluded);
    for exclusion in EXCLUSIONS {
        let _ = writeln!(out, "- {}: {}", exclusion.file, exclusion.reason);
    }

    out.push_str("\nSample failures:\n\n");
    for (name, report) in reports {
        for failure in &report.sample_failures {
            let _ = writeln!(out, "- {name}: {failure}");
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
        "| {name} | {} | {} | {checked} | {} | {:.2} | {top} |",
        report.cases,
        report.excluded,
        report.passed,
        100.0 * ratio(report.passed, checked),
    );
}
