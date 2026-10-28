//! Smoke check over a real ROM (ignored by default): emit every function analysed from its
//! vectors into the gitignored generated/ directory and compile each file with warnings as
//! errors. Needs `ANYGENESIS_ROM`; the generated C never leaves generated/.

mod support;

use std::path::PathBuf;

use anygenesis_analysis::{FlagLiveness, LivenessOptions, analyze};
use anygenesis_codegen_c::{CheckPoints, EmitOptions, emit};
use anygenesis_rom::Rom;

use support::{Optimization, compile};

#[test]
#[ignore = "needs ANYGENESIS_ROM and takes a while"]
fn test_rom_emits_and_compiles() {
    let rom_path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is set");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rom = Rom::from_bytes(std::fs::read(rom_path).expect("ROM readable")).expect("ROM loads");
    let analysis = analyze(&rom);
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    let variants = [
        ("rom-c", EmitOptions::default()),
        (
            "rom-c-hooks",
            EmitOptions {
                cycles: true,
                check_points: CheckPoints::LoopTargets,
                ..EmitOptions::default()
            },
        ),
    ];
    for (name, options) in variants {
        let output = emit(&analysis, &liveness, &options, rom.bytes());
        let lines: usize = output
            .files
            .iter()
            .map(|file| file.text.lines().count())
            .sum();
        println!(
            "{name}: {} functions, {} files, {lines} lines",
            analysis.functions.len(),
            output.files.len()
        );
        compile(
            &output,
            &workspace.join("generated").join(name),
            Optimization::Debug,
        );
    }
}
