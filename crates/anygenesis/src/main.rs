//! anygenesis: turns a Genesis ROM into a native game program on this machine.
//!
//! `anygenesis <rom> --out <folder>` translates the ROM, compiles the C with the system
//! clang and links it with the player library into one self-contained program, the whole ROM
//! image embedded, named after the ROM file, in that folder. `anygenesis <rom> [<options>]`
//! builds the same program into the per-user cache and runs it with the game's options (see
//! anygenesis-player); a cached build is reused while the ROM, anygenesis, the player
//! library and the discovery stage stay the same. Before translating, the build runs the game
//! headless on the interpreter (`--discovery profile`, the default) so the code it runs
//! becomes optimised functions; `--discovery static` or `vectors` stops earlier.
//! anygenesis itself holds no game: the player library sits next to it
//! (`ANYGENESIS_PLAYER` names another), built on its own with `cargo build --release -p
//! anygenesis-player-lib`, since building it with anygenesis would give it anygenesis's
//! interpreter. Built games are for the user who built them: never distribute one.

mod builder;
mod cache;
mod profile;
mod toolchain;

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anygenesis_patch::Patch;
use anygenesis_recomp::build;
use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

use crate::{
    cache::Entry,
    profile::Discovery,
    toolchain::{EXE_SUFFIX, PLAYER_LIBRARY, Toolchain},
};

const USAGE: &str = "usage:
  anygenesis <rom> --out <folder>   build the game program into <folder>
  anygenesis <rom> [<options>]      build it into the cache and run it with the game's options:
                                    [<frames>] [--dump <frame>[,<frame>...]] [--state <folder>]
                                    [--input <file>] [--record <file>] [--controls <file>]
  anygenesis <rom> --discovery profile|static|vectors ...
                                    either one, with how far the build discovers code
                                    (default profile: a headless run of the game finds it)
  anygenesis <rom> --patch <file> ...
                                    either one, with the ROM patched first by a patch pack
                                    (.toml) or an IPS, UPS or BPS file; repeat it to apply
                                    several in order";

/// Names the player library instead of the one next to anygenesis.
const PLAYER_VAR: &str = "ANYGENESIS_PLAYER";

fn main() -> Result<ExitCode> {
    let mut arguments = env::args_os().skip(1);
    let Some(rom_path) = arguments.next().filter(|argument| argument != "--out") else {
        bail!(USAGE);
    };
    let mut options: Vec<OsString> = arguments.collect();
    let build_options = take_build_options(&mut options)?;
    let out = match options.first() {
        Some(flag) if flag == "--out" => match &options[1..] {
            [folder] => Some(PathBuf::from(folder)),
            _ => bail!(USAGE),
        },
        _ if options.iter().any(|option| option == "--out") => bail!(USAGE),
        _ => None,
    };
    let rom_path = PathBuf::from(rom_path);
    let patches = build_options
        .patches
        .iter()
        .map(|path| {
            Patch::load(path).with_context(|| format!("loading the patch {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let rom = patched(build::load_rom(&rom_path)?, &patches)?;
    for warning in rom.warnings() {
        eprintln!("warning: {warning}");
    }
    let program = cached_program(&rom, build_options.discovery)?;
    match out {
        Some(folder) => {
            let names: Vec<&str> = patches.iter().map(Patch::name).collect();
            let exe = folder.join(exe_name(&rom_path, &names));
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

/// How the game program is built, from the options in front of the rest.
#[derive(Debug, PartialEq)]
struct BuildOptions {
    discovery: Discovery,
    /// The patches to apply to the ROM first, in order.
    patches: Vec<PathBuf>,
}

/// Takes `--discovery <stage>` (once at most, the default stage without it) and `--patch
/// <file>` (any number of times) from the front of `options`, in any order.
fn take_build_options(options: &mut Vec<OsString>) -> Result<BuildOptions> {
    let mut discovery = None;
    let mut patches = Vec::new();
    loop {
        match (
            options.first().and_then(|option| option.to_str()),
            options.get(1),
        ) {
            (Some("--discovery"), Some(stage)) if discovery.is_none() => {
                let Some(stage) = stage.to_str().and_then(Discovery::parse) else {
                    bail!(USAGE);
                };
                discovery = Some(stage);
            }
            (Some("--patch"), Some(file)) => patches.push(PathBuf::from(file)),
            (Some("--discovery" | "--patch"), _) => bail!(USAGE),
            _ => break,
        }
        options.drain(..2);
    }
    Ok(BuildOptions {
        discovery: discovery.unwrap_or_default(),
        patches,
    })
}

/// `rom` with `patches` applied, or `rom` itself without any.
fn patched(rom: Rom, patches: &[Patch]) -> Result<Rom> {
    if patches.is_empty() {
        return Ok(rom);
    }
    let names: Vec<&str> = patches.iter().map(Patch::name).collect();
    let patched = anygenesis_patch::apply(&rom, patches).with_context(|| {
        format!(
            "patching {} (SHA-1 {}) with {}",
            rom.title(),
            rom.sha1(),
            names.join(", ")
        )
    })?;
    println!(
        "{} (SHA-1 {}) patched with {}: SHA-1 {}",
        rom.title(),
        rom.sha1(),
        names.join(", "),
        patched.sha1()
    );
    Ok(patched)
}

/// The program for `rom` in the cache, discovering code up to `discovery`, built first
/// unless a current build is there.
fn cached_program(rom: &Rom, discovery: Discovery) -> Result<PathBuf> {
    let tool = env::current_exe().context("finding anygenesis's own program")?;
    let player = player_library(&tool)?;
    let entry = Entry::new(
        &cache::root()?,
        rom.sha1(),
        &tool,
        &player,
        discovery.name(),
    )?;
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
    builder::build_program(rom, &entry, &toolchain, &player, discovery)
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
             --release -p anygenesis-player-lib` on its own (built together with anygenesis it \
             gets the interpreter), or name one with {PLAYER_VAR}",
            path.display()
        );
    }
    Ok(path)
}

/// The game program's file name: the ROM file's stem, each patch's name after it in
/// brackets so a patched build never overwrites the unpatched one, with anything but
/// letters, digits and ` ._-()` replaced, so it is a plain file name on every system.
fn exe_name(rom_path: &Path, patches: &[&str]) -> String {
    let stem = rom_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name: String = patches
        .iter()
        .fold(stem, |name, patch| format!("{name} ({patch})"))
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
            let name = exe_name(Path::new(path), &[]);
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

    #[test]
    fn test_exe_name_patched_build_names_its_patches() {
        let name = exe_name(Path::new("roms/s1built.bin"), &["Fixes", "cheats: 1/2"]);
        assert_eq!(
            name.strip_suffix(EXE_SUFFIX),
            Some("s1built (Fixes) (cheats_ 1_2)")
        );
    }

    fn options(arguments: &[&str]) -> Vec<OsString> {
        arguments.iter().map(OsString::from).collect()
    }

    #[test]
    fn test_build_options_come_off_the_front_of_the_options_only() {
        let mut taken = options(&["--discovery", "static", "--out", "folder"]);
        let build = take_build_options(&mut taken).expect("a stage");
        assert_eq!(build.discovery, Discovery::Static);
        assert_eq!(build.patches, Vec::<PathBuf>::new());
        assert_eq!(taken, ["--out", "folder"]);
        let mut taken = options(&["600", "--patch", "a.ips"]);
        let build = take_build_options(&mut taken).expect("the default");
        assert_eq!(build.discovery, Discovery::Profile);
        assert_eq!(build.patches, Vec::<PathBuf>::new());
        assert_eq!(taken, ["600", "--patch", "a.ips"]);
        let mut taken = options(&["--discovery", "fast"]);
        assert!(take_build_options(&mut taken).is_err());
    }

    #[test]
    fn test_build_options_patches_kept_in_order_around_the_stage() {
        let mut taken = options(&[
            "--patch",
            "a.toml",
            "--discovery",
            "vectors",
            "--patch",
            "b.bps",
            "--out",
            "x",
        ]);
        let build = take_build_options(&mut taken).expect("options");
        assert_eq!(build.discovery, Discovery::Vectors);
        assert_eq!(
            build.patches,
            [PathBuf::from("a.toml"), PathBuf::from("b.bps")]
        );
        assert_eq!(taken, ["--out", "x"]);
        for bad in [
            &["--patch"][..],
            &["--discovery", "static", "--discovery", "static"],
        ] {
            assert!(take_build_options(&mut options(bad)).is_err(), "{bad:?}");
        }
    }
}
