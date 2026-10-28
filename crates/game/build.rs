//! Recompiles the ROM `ANYGENESIS_ROM` names: translated C into OUT_DIR, compiled with clang
//! and linked into the binary, plus `game.rs` (included by src/game.rs) describing it.
//! Nothing but the ROM decides what is built: any image the loader accepts translates, with
//! code found from its vectors and every other address as stepped code. Without
//! `ANYGENESIS_ROM` the binary builds with no game in it.

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anygenesis_recomp::build::{self, Translation};

const ROM_VAR: &str = "ANYGENESIS_ROM";
const LIBRARY: &str = "ag_game";
/// The optimisation level of the stepped files, `None` for the profile's. /O1 (clang-cl)
/// cut Sonic 1's exe by 6% and its build by 14% for about 2% of speed (M2.5, Mehran's pick).
const STEPPED_OPTIMIZATION: Option<&str> = Some("1");

fn main() -> ExitCode {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed={ROM_VAR}");
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            for line in message.lines() {
                println!("cargo::error={line}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("cargo sets OUT_DIR")?);
    let items = match env::var_os(ROM_VAR) {
        Some(rom_path) => recompile(Path::new(&rom_path), &out)?,
        None => format!(
            "/// The game compiled into this binary: none, `{ROM_VAR}` was not set.\n\
             pub fn compiled() -> Option<CompiledGame> {{\n    None\n}}\n"
        ),
    };
    let path = out.join("game.rs");
    fs::write(&path, items).map_err(|error| format!("writing {}: {error}", path.display()))
}

/// Translates and compiles the ROM at `rom_path`, and returns the Rust items describing it.
fn recompile(rom_path: &Path, out: &Path) -> Result<String, String> {
    println!("cargo::rerun-if-changed={}", rom_path.display());
    let rom = build::load_rom(rom_path).map_err(|error| format!("{ROM_VAR}: {error}"))?;
    let name = rom.title();
    let translation = build::translate(&rom, build::game_liveness(), &build::game_options());
    let directory = out.join("c");
    let failed = |error: build::BuildError| {
        format!("building {name} (SHA-1 {}) failed: {error}", rom.sha1())
    };
    let sources = build::write_output(&translation.output, &directory).map_err(failed)?;
    build::compile_game(LIBRARY, &directory, &sources, STEPPED_OPTIMIZATION).map_err(failed)?;
    Ok(game_items(&name, &translation))
}

/// The Rust items that reach the compiled program, `name` the game's title.
fn game_items(name: &str, translation: &Translation) -> String {
    let analysis = &translation.analysis;
    let mut handlers = String::new();
    for entry in build::vector_handlers(analysis) {
        write!(handlers, "0x{entry:06X}, ").expect("writing to a String cannot fail");
    }
    let mut entries = String::new();
    for entry in analysis.functions.keys() {
        write!(entries, "0x{:06X}, ", entry.get()).expect("writing to a String cannot fail");
    }
    format!(
        "unsafe extern \"C\" {{\n    \
         static ag_table: anygenesis_abi::AgEntry;\n    \
         static ag_table_size: u32;\n    \
         static ag_steps: Option<anygenesis_abi::AgFn>;\n    \
         static ag_steps_size: u32;\n    \
         fn ag_step_entry(state: *mut anygenesis_abi::AgState, pc: u32) -> u32;\n    \
         fn ag_interrupt(state: *mut anygenesis_abi::AgState, level: u32, pc: u32) -> u32;\n\
         }}\n\n\
         /// The game compiled into this binary.\n\
         pub fn compiled() -> Option<CompiledGame> {{\n    \
         // SAFETY: the build script linked the program, whose table holds `ag_table_size`\n    \
         // rows from `ag_table` on, and whose stepped table `ag_steps_size` entries from\n    \
         // `ag_steps` on; neither ever changes.\n    \
         let (table, steps) = unsafe {{\n        \
         (\n            \
         std::slice::from_raw_parts(&raw const ag_table, ag_table_size as usize),\n            \
         std::slice::from_raw_parts(&raw const ag_steps, ag_steps_size as usize),\n        \
         )\n    }};\n    \
         Some(CompiledGame {{\n        \
         name: {name:?},\n        \
         sha1: {sha1:?},\n        \
         functions: {functions},\n        \
         table,\n        \
         steps,\n        \
         step_entry: ag_step_entry,\n        \
         interrupt: ag_interrupt,\n        \
         vector_handlers: &[{handlers}],\n        \
         function_entries: &[{entries}],\n    }})\n}}\n",
        sha1 = analysis.sha1,
        functions = analysis.functions.len(),
    )
}
