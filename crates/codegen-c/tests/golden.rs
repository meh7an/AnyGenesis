//! Golden files for synthetic programs, hand-assembled word by word, and a clang compile of
//! every emitted file with warnings as errors. `UPDATE_GOLDEN=1` rewrites the golden files.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anygenesis_analysis::{Analysis, EntryKind, FlagLiveness, Hints, LivenessOptions};
use anygenesis_codegen_c::{
    CheckPoints, EmitOptions, LineMap, Output, OutputFile, emit, emit_stepped_instruction,
};
use anygenesis_shared::Address;

use support::{Image, Optimization, RESET, branch_w, compile, function, scratch, table};

const RTS: u16 = 0x4E75;

const F: u32 = 0x400;
const G: u32 = 0x500;
const H: u32 = 0x600;
const N: u32 = 0x700;

/// reset: bsr.w f; jsr g; tst.w d0; beq.s skip; addq.w #1,d2; skip: bra.w h (a tail jump:
/// h is a hinted function). f: a dbf loop. g: memory accesses of every region. h: rts.
fn calls() -> (Image, Hints) {
    let mut reset = branch_w(0x6100, RESET, F).to_vec();
    reset.extend([0x4EB9, 0x0000, G as u16, 0x4A40, 0x6702, 0x5242]);
    reset.extend(branch_w(0x6000, 0x310, H));
    let image = Image::new()
        .words(RESET, &reset)
        .words(F, &[0x7003, 0x5242, 0x51C8, 0xFFFC, RTS])
        .words(G, &memory_accesses())
        .words(H, &[RTS]);
    let hints = Hints {
        functions: vec![function(H, false)],
        ..Hints::default()
    };
    (image, hints)
}

/// move.w d0,($8000).w (work RAM at a constant address); move.w ($0400).w,d1 (ROM at a
/// constant address); move.b (a1),d3 (any address); move.w d1,($A10000).l (I/O at a constant
/// address); move.l d3,-(a7); addq.l #4,a7; rts.
fn memory_accesses() -> Vec<u16> {
    vec![
        0x31C0, 0x8000, 0x3238, 0x0400, 0x1611, 0x33C1, 0x00A1, 0x0000, 0x2F03, 0x588F, RTS,
    ]
}

/// reset: a word-offset jump table into `jsr (a1)` (a long table of two functions) and
/// `jmp (a0)` (unresolved).
fn tables() -> (Image, Hints) {
    let image = Image::new()
        .words(
            RESET,
            &[
                0x323B, 0x0006, 0x4EFB, 0x1002, 0x0004, 0x0006, 0x4E91, 0x4ED0,
            ],
        )
        .words(0x400, &[0x0000, 0x0410, 0x0000, 0x0420])
        .words(0x410, &[RTS])
        .words(0x420, &[0x7000, RTS]);
    let hints = Hints {
        jump_tables: vec![
            table(0x308, 2, EntryKind::WordOffset, None),
            table(0x400, 2, EntryKind::Long, Some(0x30C)),
        ],
        ..Hints::default()
    };
    (image, hints)
}

/// reset: trap #0; move.w d0,sr (privileged); divu.w d1,d0 (divide by zero); stop #$2700;
/// reset; move.w sr,d2; bsr.w n, hinted as never returning: moveq #1,d0 and `bra.s *`. A
/// hinted function at 0x800 is only `bra.s *`, touching no state.
fn exceptions() -> (Image, Hints) {
    let mut reset = vec![0x4E40, 0x46C0, 0x80C1, 0x4E72, 0x2700, 0x4E70, 0x40C2];
    reset.extend(branch_w(0x6100, 0x30E, N));
    let image = Image::new()
        .words(RESET, &reset)
        .words(N, &[0x7001, 0x60FE])
        .words(0x800, &[0x60FE]);
    let hints = Hints {
        functions: vec![function(N, true), function(0x800, false)],
        ..Hints::default()
    };
    (image, hints)
}

fn emit_program(program: (Image, Hints), options: &EmitOptions) -> Output {
    let (image, hints) = program;
    let analysis: Analysis = image.analyze(&hints);
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    emit(&analysis, &liveness, options, &image.bytes)
}

/// Every instruction of `program` on its own listing line, from line 10.
fn line_map(program: &(Image, Hints)) -> LineMap {
    let analysis = program.0.analyze(&program.1);
    let lines: BTreeMap<_, _> = analysis
        .instructions
        .keys()
        .enumerate()
        .map(|(index, &address)| (address, 10 + index as u32))
        .collect();
    LineMap {
        file: "synthetic.asm".to_owned(),
        lines,
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

/// Compares `actual` with the golden file `name`, or rewrites it under `UPDATE_GOLDEN=1`.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden files live in a directory"))
            .expect("golden directory created");
        std::fs::write(&path, actual).expect("golden file written");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error} (run with UPDATE_GOLDEN=1)", path.display()));
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(left, right)| left != right)
            .map_or_else(
                || "the end".to_owned(),
                |index| format!("line {}", index + 1),
            );
        panic!(
            "{} differs from the emitted text at {line}; rerun with UPDATE_GOLDEN=1 and review \
             the diff",
            path.display()
        );
    }
}

/// The program's own files (headers shared by every program excluded), one after another.
/// Of the stepped files only `ag_step_entry` and `ag_interrupt` are kept: the superset covers
/// every even address of the 4 KiB image, so the golden file for single stepped
/// instructions shows their form.
fn program_text(output: &Output) -> String {
    let mut text = String::new();
    for file in &output.files {
        let body = match file.name.as_str() {
            "ag_state.h" | "ag_runtime.h" => continue,
            "ag_steps.c" => {
                let start = file
                    .text
                    .find("uint32_t ag_step_entry(")
                    .expect("ag_steps.c holds ag_step_entry");
                &file.text[start..]
            }
            name if name.starts_with("ag_s") => continue,
            _ => file.text.as_str(),
        };
        text.push_str(&format!("/* ==== {} ==== */\n", file.name));
        text.push_str(body);
    }
    text
}

fn check(name: &str, output: &Output) {
    assert_golden(&format!("{name}.c"), &program_text(output));
    let directory = scratch(name);
    compile(output, &directory.join("debug"), Optimization::Debug);
    compile(output, &directory.join("release"), Optimization::Release);
}

#[test]
fn test_golden_calls() {
    check("calls", &emit_program(calls(), &EmitOptions::default()));
}

#[test]
fn test_golden_calls_with_cycles_check_points_stepped_code_and_lines() {
    let program = calls();
    let options = EmitOptions {
        cycles: true,
        check_points: CheckPoints::BlockStarts,
        stepped: true,
        lines: Some(line_map(&program)),
        ..EmitOptions::default()
    };
    let output = emit_program(program, &options);
    let stats = output.stepped.expect("stepped code was asked for");
    // Every analysed instruction has a stepped function, and every decodable address is
    // kept or pruned.
    assert_eq!(stats.kept + stats.pruned, stats.decodable);
    assert!(stats.kept >= 10, "{stats:?}");
    assert!(stats.fast > 0, "{stats:?}");
    check("calls_options", &output);
}

/// One instruction of each stepped form: the fast path (moveq, dbf, rts), a device access
/// (move.w d0 to the VDP data port), a call (bsr.w), an SR write (move.w d0,sr), an
/// exception (trap #0) and STOP.
#[test]
fn test_golden_stepped_instructions() {
    let instructions: [(u32, &[u16]); 8] = [
        (0x400, &[0x7001]),
        (0x402, &[0x51C8, 0xFFFC]),
        (0x406, &[RTS]),
        (0x408, &[0x33C0, 0x00C0, 0x0000]),
        (0x40E, &[0x6100, 0x0100]),
        (0x412, &[0x46C0]),
        (0x414, &[0x4E40]),
        (0x416, &[0x4E72, 0x2700]),
    ];
    let mut text = String::from("#include \"ag_runtime.h\"\n");
    for (address, words) in instructions {
        let mut padded = words.to_vec();
        padded.resize(anygenesis_m68k::MAX_WORDS, 0);
        let instruction = anygenesis_m68k::decode(&padded).expect("the words decode");
        let lifted =
            anygenesis_ir::lift(&instruction, Address::new(address)).expect("the words lift");
        text.push('\n');
        text.push_str(&emit_stepped_instruction(&lifted, 0x1000, ""));
    }
    assert_golden("stepped.c", &text);
    let headers = emit_program(calls(), &EmitOptions::default());
    let mut files: Vec<OutputFile> = headers
        .files
        .into_iter()
        .filter(|file| file.name.ends_with(".h"))
        .collect();
    files.push(OutputFile {
        name: "stepped.c".to_owned(),
        text,
    });
    let output = Output {
        files,
        table_rows: 0,
        stepped: None,
    };
    compile(&output, &scratch("stepped"), Optimization::Debug);
}

#[test]
fn test_golden_tables() {
    check("tables", &emit_program(tables(), &EmitOptions::default()));
}

#[test]
fn test_golden_exceptions() {
    check(
        "exceptions",
        &emit_program(exceptions(), &EmitOptions::default()),
    );
}

#[test]
fn test_golden_exceptions_with_loop_check_points() {
    let options = EmitOptions {
        cycles: true,
        check_points: CheckPoints::LoopTargets,
        ..EmitOptions::default()
    };
    check("exceptions_options", &emit_program(exceptions(), &options));
}

#[test]
fn test_golden_state_header() {
    let output = emit_program(calls(), &EmitOptions::default());
    let header = output
        .files
        .iter()
        .find(|file| file.name == "ag_state.h")
        .expect("the state header is emitted");
    assert_golden("ag_state.h", &header.text);
}

#[test]
fn test_chunks_split_at_the_line_budget() {
    let options = EmitOptions {
        chunk_lines: 1,
        ..EmitOptions::default()
    };
    let output = emit_program(calls(), &options);
    let names: Vec<&str> = output.sources().map(|file| file.name.as_str()).collect();
    // The rte handler, reset, f, g and h: one file each, then the table.
    assert_eq!(
        names,
        [
            "ag_000.c",
            "ag_001.c",
            "ag_002.c",
            "ag_003.c",
            "ag_004.c",
            "ag_table.c"
        ]
    );
    compile(&output, &scratch("chunks"), Optimization::Debug);
}

/// Guards the compile check itself: a warning must fail it.
#[test]
#[should_panic(expected = "failed to compile")]
fn test_compile_check_rejects_warnings() {
    let output = Output {
        files: vec![OutputFile {
            name: "warning.c".to_owned(),
            text: "int unused_variable(void)\n{\n    int x;\n    return 0;\n}\n".to_owned(),
        }],
        table_rows: 0,
        stepped: None,
    };
    compile(&output, &scratch("warning"), Optimization::Debug);
}

#[test]
fn test_symbol_prefix_renames_every_defined_symbol() {
    let plain = emit_program(calls(), &EmitOptions::default());
    let options = EmitOptions {
        symbol_prefix: "p7_".to_owned(),
        ..EmitOptions::default()
    };
    let prefixed = emit_program(calls(), &options);
    // Every name the output defines starts with `f_` or `ag_table`, or is the guard; the
    // externs and helpers (`ag_lookup`, `ag_load16`, ...) keep their names.
    let renamed = program_text(&plain)
        .replace("f_", "p7_f_")
        .replace("ag_table[]", "p7_ag_table[]")
        .replace("ag_table_size", "p7_ag_table_size")
        .replace("AG_FUNCTIONS_H", "P7_AG_FUNCTIONS_H");
    assert_eq!(program_text(&prefixed), renamed);
    compile(&prefixed, &scratch("prefix"), Optimization::Debug);
}

#[test]
fn test_emission_is_deterministic() {
    let first = emit_program(tables(), &EmitOptions::default());
    let second = emit_program(tables(), &EmitOptions::default());
    assert_eq!(first, second);
}
