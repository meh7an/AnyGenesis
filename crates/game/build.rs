//! Recompiles the ROM `ANYGENESIS_ROM` names: the program anygenesis builds a game exe from
//! (translated C, the embedded image and the `ag_game` table) into OUT_DIR, compiled with
//! clang and linked into the binary, plus `game.rs` (included by src/game.rs) reaching the
//! table. Nothing but the ROM decides what is built: any image the loader accepts
//! translates, with code found from its vectors and every other address as stepped code.
//! Without `ANYGENESIS_ROM` the binary builds with no game in it.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anygenesis_recomp::build;

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
            "/// The table of the game compiled into this binary: none, `{ROM_VAR}` was not \
             set.\n\
             pub fn table() -> Option<&'static anygenesis_abi::AgGame> {{\n    None\n}}\n"
        ),
    };
    let path = out.join("game.rs");
    fs::write(&path, items).map_err(|error| format!("writing {}: {error}", path.display()))
}

/// The Rust items reaching the program's `ag_game` table.
const GAME_ITEMS: &str = "unsafe extern \"C\" {\n    \
                          static ag_game: anygenesis_abi::AgGame;\n}\n\n\
                          /// The table of the game compiled into this binary.\n\
                          pub fn table() -> Option<&'static anygenesis_abi::AgGame> {\n    \
                          // SAFETY: the build script linked the program, which defines \
                          `ag_game`\n    \
                          // in its read-only data, where it never changes.\n    \
                          Some(unsafe { &ag_game })\n}\n";

/// Translates and compiles the ROM at `rom_path`, and returns the Rust items reaching it.
fn recompile(rom_path: &Path, out: &Path) -> Result<String, String> {
    println!("cargo::rerun-if-changed={}", rom_path.display());
    let rom = build::load_rom(rom_path).map_err(|error| format!("{ROM_VAR}: {error}"))?;
    let name = rom.title();
    let translation = build::translate(&rom, build::game_liveness(), &build::game_options());
    let directory = out.join("c");
    let failed = |error: build::BuildError| {
        format!("building {name} (SHA-1 {}) failed: {error}", rom.sha1())
    };
    let sources = build::write_program(&rom, &translation, &[], &directory).map_err(failed)?;
    build::compile_game(LIBRARY, &directory, &sources, STEPPED_OPTIMIZATION).map_err(failed)?;
    Ok(GAME_ITEMS.to_owned())
}
