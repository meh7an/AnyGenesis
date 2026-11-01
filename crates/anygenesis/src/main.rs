//! anygenesis: turns a Genesis ROM into a native game program on this machine.
//!
//! `anygenesis <rom> --out <folder>` translates the ROM, compiles the C with the bundled Zig
//! and links it with the player library into one self-contained program, the whole ROM image
//! embedded, named after the ROM file, in that folder. `anygenesis <rom> [<options>]` builds
//! the same program into the per-user cache and runs it with the game's options (see
//! anygenesis-player); a cached build is reused while the ROM, anygenesis, the player
//! library, the Zig version, the discovery stage and the patch packs' code patches stay the
//! same. Before translating, the build runs the game headless on the interpreter
//! (`--discovery profile`, the default) so the code it runs becomes optimised functions;
//! `--discovery static` or `vectors` stops earlier. anygenesis itself holds no game: Zig and
//! the player library are in the toolchain folder next to it (`ANYGENESIS_TOOLCHAIN` names
//! another), which `cargo xtask toolchain` builds, so nothing else needs installing. Built
//! games are for the user who built them: never distribute one. `--progress` adds
//! machine-readable progress lines for anygenesis-gui (see `progress`).

mod builder;
mod cache;
mod profile;
mod progress;
mod toolchain;

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anygenesis_patch::{CodePatches, Patch};
use anygenesis_recomp::build;
use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

use crate::{
    cache::Entry,
    profile::Discovery,
    progress::Phase,
    toolchain::{EXE_SUFFIX, Toolchain},
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
                                    (.toml, which may also carry C hooks, RAM patches and
                                    flags) or an IPS, UPS or BPS file; repeat it to apply
                                    several in order
  anygenesis <rom> --progress ...   either one, with machine-readable progress lines
  anygenesis --version              print the version";

fn main() -> Result<ExitCode> {
    run().inspect_err(progress::error)
}

fn run() -> Result<ExitCode> {
    let mut arguments = env::args_os().skip(1);
    let Some(rom_path) = arguments.next().filter(|argument| argument != "--out") else {
        bail!(USAGE);
    };
    if rom_path == "--version" {
        println!("anygenesis {}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    let mut options: Vec<OsString> = arguments.collect();
    let build_options = take_build_options(&mut options)?;
    if build_options.progress {
        progress::enable();
    }
    let out = match options.first() {
        Some(flag) if flag == "--out" => match &options[1..] {
            [folder] => Some(PathBuf::from(folder)),
            _ => bail!(USAGE),
        },
        _ if options.iter().any(|option| option == "--out") => bail!(USAGE),
        _ => None,
    };
    let rom_path = PathBuf::from(rom_path);
    progress::phase(Phase::Read);
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
    let code = CodePatches::of(&patches)?;
    if !code.is_empty() {
        println!(
            "code patches: {} in C, {} RAM, {} flags",
            code.patches.len(),
            code.ram.len(),
            code.flags.set().len()
        );
    }
    let program = cached_program(&rom, build_options.discovery, &code)?;
    match out {
        Some(folder) => {
            let names: Vec<&str> = patches.iter().map(Patch::name).collect();
            let exe = folder.join(exe_name(&rom_path, &names));
            fs::create_dir_all(&folder)
                .with_context(|| format!("creating {}", folder.display()))?;
            fs::copy(&program, &exe).with_context(|| format!("writing {}", exe.display()))?;
            println!("{} written to {}", rom.title(), exe.display());
            progress::done(exe.display());
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
    /// Whether to print the progress lines.
    progress: bool,
}

/// Takes `--discovery <stage>` (once at most, the default stage without it) and `--patch
/// <file>` (any number of times) and `--progress` from the front of `options`, in any order.
fn take_build_options(options: &mut Vec<OsString>) -> Result<BuildOptions> {
    let mut discovery = None;
    let mut patches = Vec::new();
    let mut progress = false;
    loop {
        if options.first().is_some_and(|option| option == "--progress") {
            progress = true;
            options.remove(0);
            continue;
        }
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
        progress,
    })
}

/// `rom` with `patches` applied, or `rom` itself without any.
fn patched(rom: Rom, patches: &[Patch]) -> Result<Rom> {
    if patches.is_empty() {
        return Ok(rom);
    }
    progress::phase(Phase::Patch);
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

/// The program for `rom` in the cache, discovering code up to `discovery`, with the code
/// patches `code`, built first unless a current build is there.
fn cached_program(rom: &Rom, discovery: Discovery, code: &CodePatches) -> Result<PathBuf> {
    let tool = env::current_exe().context("finding anygenesis's own program")?;
    let root = cache::root()?;
    let toolchain = Toolchain::find(&tool, &cache::zig_folder(&root))?;
    let entry = Entry::new(
        &root,
        rom.sha1(),
        &tool,
        toolchain.player(),
        toolchain.version(),
        discovery.name(),
        (!code.is_empty()).then(|| code.hash()),
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
    println!(
        "{} (SHA-1 {}): building in {} with Zig {}",
        rom.title(),
        rom.sha1(),
        entry.folder.display(),
        toolchain.version()
    );
    builder::build_program(rom, &entry, &toolchain, discovery, code)
        .with_context(|| format!("building {} (SHA-1 {})", rom.title(), rom.sha1()))?;
    Ok(program)
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
        assert!(!build.progress);
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
        let mut taken = options(&["--progress", "--patch", "a.ips", "--progress", "--out", "x"]);
        let build = take_build_options(&mut taken).expect("options");
        assert!(build.progress);
        assert_eq!(build.patches, [PathBuf::from("a.ips")]);
        assert_eq!(taken, ["--out", "x"]);
        for bad in [
            &["--patch"][..],
            &["--discovery", "static", "--discovery", "static"],
        ] {
            assert!(take_build_options(&mut options(bad)).is_err(), "{bad:?}");
        }
    }
}
