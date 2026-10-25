//! The recompiled game: translated 68000 code linked against the runtime.
//!
//! Until the runtime exists the externs are stubs, so the binary loads and checks the ROM,
//! checks the translated program's address table, prepares the reset state and stops
//! before running any 68000 code.

mod game;
mod stub;

use std::{env, path::PathBuf};

use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

const ROM_VAR: &str = "ANYGENESIS_ROM";

fn main() -> Result<()> {
    let Some(game) = game::compiled() else {
        bail!("built without a game: set GAME and {ROM_VAR}, then rebuild");
    };
    let path = rom_path()?;
    let rom = Rom::load(&path).with_context(|| format!("loading {}", path.display()))?;
    let sha1 = rom.sha1().to_string();
    if sha1 != game.sha1 {
        bail!(
            "{} has SHA-1 {sha1}, but this build of {} was translated from {}",
            path.display(),
            game.name,
            game.sha1
        );
    }
    let state = stub::reset_state(&rom);
    game.check_table(state.pc)?;
    println!("game {}, ROM SHA-1 {sha1}", game.name);
    println!(
        "{} functions, {} table rows, {} vector handlers",
        game.functions,
        game.table.len(),
        game.vector_handlers.len()
    );
    println!("reset: SSP ${:08X}, PC ${:06X}", state.a[7], state.pc);
    println!("stub runtime: stopping before the first instruction");
    Ok(())
}

/// The ROM image: the first argument, else `ANYGENESIS_ROM`.
fn rom_path() -> Result<PathBuf> {
    env::args_os()
        .nth(1)
        .or_else(|| env::var_os(ROM_VAR))
        .map(PathBuf::from)
        .with_context(|| format!("usage: anygenesis <rom.bin> (or set {ROM_VAR})"))
}
