//! Smoke check over the real ROM (ignored by default): emit every analysed Sonic 1 function
//! into the gitignored generated/ directory and compile each file with warnings as errors.
//! Needs `ANYGENESIS_ROM`; the generated C never leaves generated/.

mod support;

use std::path::PathBuf;

use anygenesis_analysis::{FlagLiveness, LivenessOptions, analyze};
use anygenesis_codegen_c::{CheckPoints, EmitOptions, emit};
use anygenesis_gamecfg::GameConfig;
use anygenesis_rom::Rom;

use support::{Optimization, compile};

#[test]
#[ignore = "needs ANYGENESIS_ROM and takes a while"]
fn test_sonic1_emits_and_compiles() {
    let rom_path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is set");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rom = Rom::from_bytes(std::fs::read(rom_path).expect("ROM readable")).expect("ROM loads");
    let config_text =
        std::fs::read_to_string(workspace.join("games/sonic1.toml")).expect("config readable");
    let config = GameConfig::parse(&config_text).expect("config parses");
    let analysis = analyze(&rom, &config).expect("config matches the ROM");
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    let variants = [
        ("sonic1-c", EmitOptions::default()),
        (
            "sonic1-c-hooks",
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
