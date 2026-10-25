//! Recompiles the game `GAME` names from the ROM `ANYGENESIS_ROM` names: translated C into
//! OUT_DIR, compiled with clang and linked into the binary, plus `game.rs` (included by
//! src/game.rs) describing it. Without `GAME` the binary builds with no game in it.

use std::{
    env,
    ffi::OsString,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anygenesis_recomp::build::{self, Translation};

const GAME_VAR: &str = "GAME";
const ROM_VAR: &str = "ANYGENESIS_ROM";
const LIBRARY: &str = "ag_game";

fn main() -> ExitCode {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed={GAME_VAR}");
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
    let items = match env::var_os(GAME_VAR) {
        Some(game) => recompile(&game, &out)?,
        None => "/// The game compiled into this binary: none, `GAME` was not set.\n\
                 pub fn compiled() -> Option<CompiledGame> {\n    None\n}\n"
            .to_owned(),
    };
    let path = out.join("game.rs");
    fs::write(&path, items).map_err(|error| format!("writing {}: {error}", path.display()))
}

/// Translates and compiles the game, and returns the Rust items describing it.
fn recompile(game: &OsString, out: &Path) -> Result<String, String> {
    let game = game
        .to_str()
        .ok_or_else(|| format!("{GAME_VAR}={game:?} is not UTF-8"))?;
    let config_path = build::config_path(game).map_err(|error| format!("{GAME_VAR}: {error}"))?;
    println!("cargo::rerun-if-changed={}", config_path.display());
    let rom_path = env::var_os(ROM_VAR).ok_or_else(|| {
        format!("{GAME_VAR}={game} needs its ROM: set {ROM_VAR} to the path of the .bin image")
    })?;
    let rom_path = PathBuf::from(rom_path);
    println!("cargo::rerun-if-changed={}", rom_path.display());
    let config = build::load_config(&config_path).map_err(|error| error.to_string())?;
    let rom = build::load_rom(&rom_path).map_err(|error| format!("{ROM_VAR}: {error}"))?;
    let translation = build::translate(
        &rom,
        &config,
        build::game_liveness(),
        &build::game_options(),
    )
    .map_err(|error| format!("{ROM_VAR} does not fit games/{game}.toml: {error}"))?;
    let directory = out.join("c");
    let sources =
        build::write_output(&translation.output, &directory).map_err(|error| error.to_string())?;
    build::compile(LIBRARY, &directory, &sources).map_err(|error| error.to_string())?;
    Ok(game_items(game, &translation))
}

/// The Rust items that reach the compiled program.
fn game_items(game: &str, translation: &Translation) -> String {
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
         static ag_table_size: u32;\n}}\n\n\
         /// The game compiled into this binary.\n\
         pub fn compiled() -> Option<CompiledGame> {{\n    \
         // SAFETY: the build script linked the program, whose table holds `ag_table_size`\n    \
         // rows from `ag_table` on and never changes.\n    \
         let table = unsafe {{\n        \
         std::slice::from_raw_parts(&raw const ag_table, ag_table_size as usize)\n    }};\n    \
         Some(CompiledGame {{\n        \
         name: {game:?},\n        \
         sha1: {sha1:?},\n        \
         functions: {functions},\n        \
         table,\n        \
         vector_handlers: &[{handlers}],\n        \
         function_entries: &[{entries}],\n    }})\n}}\n",
        sha1 = analysis.sha1,
        functions = analysis.functions.len(),
    )
}
