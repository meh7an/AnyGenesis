//! Analyses the real ROM named by `ANYGENESIS_ROM` from its vectors and writes the
//! report to `target/tmp/analysis-report.md` for a manual review. Run with
//! `cargo test -p anygenesis-analysis --test real_rom -- --ignored --nocapture`.

use std::path::Path;

use anygenesis_analysis::analyze;
use anygenesis_rom::Rom;
use anygenesis_shared::ROM_MARKER;

#[test]
#[ignore = "needs ANYGENESIS_ROM pointing at a ROM image"]
fn test_analyze_real_rom_writes_report() {
    let path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is not set");
    let rom = Rom::load(&path).expect("ANYGENESIS_ROM loads");
    let analysis = analyze(&rom);

    let report_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("analysis-report.md");
    let report = format!("{ROM_MARKER}\n\n{}", analysis.report());
    std::fs::write(&report_path, report).expect("report is writable");
    println!("report          {}", report_path.display());
    println!("functions       {}", analysis.functions.len());
    println!("blocks          {}", analysis.blocks.len());
    println!("shared blocks   {}", analysis.shared_blocks().count());
    println!("instructions    {}", analysis.instructions.len());
    println!("unresolved      {}", analysis.unresolved.len());
    println!("diagnostics     {}", analysis.diagnostics.len());

    assert!(!analysis.functions.is_empty());
}
