//! Analyses the real ROM named by `ANYGENESIS_ROM` with `games/sonic1.toml` and writes the
//! report to `target/tmp/analysis-report.md` for a manual review. Run with
//! `cargo test -p anygenesis-analysis --test real_rom -- --ignored --nocapture`.

use std::path::Path;

use anygenesis_analysis::analyze;
use anygenesis_gamecfg::GameConfig;
use anygenesis_rom::Rom;

#[test]
#[ignore = "needs ANYGENESIS_ROM pointing at the Sonic 1 REV00 .bin"]
fn test_analyze_real_rom_sonic1_writes_report() {
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

    let report_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("analysis-report.md");
    std::fs::write(&report_path, analysis.report().to_string()).expect("report is writable");
    println!("report          {}", report_path.display());
    println!("functions       {}", analysis.functions.len());
    println!("blocks          {}", analysis.blocks.len());
    println!("shared blocks   {}", analysis.shared_blocks().count());
    println!("instructions    {}", analysis.instructions.len());
    println!("unresolved      {}", analysis.unresolved.len());
    println!("diagnostics     {}", analysis.diagnostics.len());

    assert!(!analysis.functions.is_empty());
}
