//! anygenesis: turns a Genesis ROM into a native game program on this machine.
//!
//! `anygenesis <rom> --out <folder>` translates the ROM, compiles the C with the system
//! clang and links it with the player library into one self-contained program, the whole ROM
//! image embedded, named after the ROM file, in that folder. `anygenesis <rom> [<options>]`
//! builds the same program into the per-user cache and runs it with the game's options (see
//! anygenesis-player); a cached build is reused while the ROM, anygenesis and the player
//! library stay the same. anygenesis itself holds no game: the player library sits next to
//! it (`ANYGENESIS_PLAYER` names another), built with `cargo build --release -p
//! anygenesis-player-lib`. Built games are for the user who built them: never distribute
//! one.

mod builder;
mod cache;
mod toolchain;

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anygenesis_recomp::build;
use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

use crate::{
    cache::Entry,
    toolchain::{EXE_SUFFIX, PLAYER_LIBRARY, Toolchain},
};

const USAGE: &str = "usage:
  anygenesis <rom> --out <folder>   build the game program into <folder>
  anygenesis <rom> [<options>]      build it into the cache and run it with the game's options:
                                    [<frames>] [--dump <frame>[,<frame>...]] [--state <folder>]
                                    [--input <file>] [--record <file>] [--controls <file>]";

/// Names the player library instead of the one next to anygenesis.
const PLAYER_VAR: &str = "ANYGENESIS_PLAYER";

fn main() -> Result<ExitCode> {
    let mut arguments = env::args_os().skip(1);
    let Some(rom_path) = arguments.next().filter(|argument| argument != "--out") else {
        bail!(USAGE);
    };
    let options: Vec<OsString> = arguments.collect();
    let out = match options.first() {
        Some(flag) if flag == "--out" => match &options[1..] {
            [folder] => Some(PathBuf::from(folder)),
            _ => bail!(USAGE),
        },
        _ if options.iter().any(|option| option == "--out") => bail!(USAGE),
        _ => None,
    };
    let rom_path = PathBuf::from(rom_path);
    let rom = build::load_rom(&rom_path)?;
    for warning in rom.warnings() {
        eprintln!("warning: {warning}");
    }
    let program = cached_program(&rom)?;
    match out {
        Some(folder) => {
            let exe = folder.join(exe_name(&rom_path));
            fs::create_dir_all(&folder)
                .with_context(|| format!("creating {}", folder.display()))?;
            fs::copy(&program, &exe).with_context(|| format!("writing {}", exe.display()))?;
            println!("{} written to {}", rom.title(), exe.display());
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let status = Command::new(&program)
                .args(&options)
                .status()
                .with_context(|| format!("running {}", program.display()))?;
            Ok(match status.code() {
                Some(0) => ExitCode::SUCCESS,
                Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1).max(1)),
                None => ExitCode::FAILURE,
            })
        }
    }
}

/// The program for `rom` in the cache, built first unless a current build is there.
fn cached_program(rom: &Rom) -> Result<PathBuf> {
    let tool = env::current_exe().context("finding anygenesis's own program")?;
    let player = player_library(&tool)?;
    let entry = Entry::new(&cache::root()?, rom.sha1(), &tool, &player)?;
    let program = builder::program(&entry);
    if entry.is_current() && program.is_file() {
        println!(
            "{}: using the cached build in {}",
            rom.title(),
            entry.folder.display()
        );
        return Ok(program);
    }
    let toolchain = Toolchain::find()?;
    println!(
        "{} (SHA-1 {}): building in {} with {}",
        rom.title(),
        rom.sha1(),
        entry.folder.display(),
        toolchain.compiler().display()
    );
    builder::build_program(rom, &entry, &toolchain, &player)
        .with_context(|| format!("building {} (SHA-1 {})", rom.title(), rom.sha1()))?;
    Ok(program)
}

/// The player library: `ANYGENESIS_PLAYER`, else the one next to anygenesis at `tool`.
fn player_library(tool: &Path) -> Result<PathBuf> {
    let path = match env::var_os(PLAYER_VAR) {
        Some(path) => PathBuf::from(path),
        None => tool
            .parent()
            .context("anygenesis's program has no folder")?
            .join(PLAYER_LIBRARY),
    };
    if !path.is_file() {
        bail!(
            "the player library {} is missing: build it next to anygenesis with `cargo build \
             --release -p anygenesis -p anygenesis-player-lib`, or name one with {PLAYER_VAR}",
            path.display()
        );
    }
    Ok(path)
}

/// The game program's file name: the ROM file's stem with anything but letters, digits and
/// ` ._-()` replaced, so it is a plain file name on every system.
fn exe_name(rom_path: &Path) -> String {
    let stem = rom_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name: String = stem
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || " ._-()".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim();
    format!(
        "{}{EXE_SUFFIX}",
        if name.is_empty() { "game" } else { name }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exe_name_keeps_the_rom_stem_and_replaces_odd_characters() {
        let name = |path: &str| {
            let name = exe_name(Path::new(path));
            name.strip_suffix(EXE_SUFFIX).unwrap_or(&name).to_owned()
        };
        assert_eq!(name("roms/s1built.bin"), "s1built");
        assert_eq!(
            name("Sonic The Hedgehog (USA, Europe).md"),
            "Sonic The Hedgehog (USA_ Europe)"
        );
        assert_eq!(name("a<b*c.bin"), "a_b_c");
        assert_eq!(name(".bin"), ".bin");
        assert_eq!(name(""), "game");
    }
}
