//! Legal guard: keeps ROM images, other large binaries, the external reference emulator's
//! programs and scripts, and per-game hint files out of the repository.

use std::{
    ffi::OsStr,
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};

use crate::workspace_root;

/// Nothing hand-written in this repository comes near this size; ROM images all exceed it.
const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// `.md` is a Genesis ROM extension too, but it collides with Markdown, so ROM-shaped `.md`
/// files are caught by the header check instead.
const ROM_EXTENSIONS: [&str; 3] = ["bin", "gen", "smd"];

/// Every Genesis cartridge header starts with its system type at this offset.
const HEADER_OFFSET: usize = 0x100;
const HEADER_MAGIC: &[u8] = b"SEGA";
const HEAD_LEN: usize = HEADER_OFFSET + HEADER_MAGIC.len();

/// Programs and libraries: the reference emulator (and any other tool) runs from outside the
/// repository, so none is ever committed.
const NATIVE_EXTENSIONS: [&str; 4] = ["exe", "dll", "so", "dylib"];
/// Windows (MZ) and ELF executables and libraries start with these.
const NATIVE_MAGICS: [&[u8]; 2] = [b"MZ", b"\x7FELF"];

/// The first line of every reference emulator debugger script the frame diff writes; the
/// scripts and the dumps they save stay outside the repository.
const SCRIPT_MAGIC: &[u8] = b"breakpoint $";

/// Games build from their ROM alone (Architecture Decision 7): the folder per-game configs
/// lived in stays gone, and no TOML file anywhere holds a hint section of the old format.
const GAMES_FOLDER: &str = "games/";
const HINT_SECTIONS: [&str; 5] = ["jump_table", "function", "indirect", "unreached", "data"];

/// A rule a candidate file breaks.
#[derive(Debug, PartialEq, Eq)]
enum Violation {
    TooLarge { size: u64 },
    RomExtension,
    RomHeader,
    NativeBinary,
    DebuggerScript,
    GamesFolder,
    HintSection { name: &'static str },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { size } => {
                write!(f, "is {size} bytes, over the {MAX_FILE_SIZE}-byte limit")
            }
            Self::RomExtension => write!(f, "has a ROM image extension (.bin, .gen, .smd)"),
            Self::RomHeader => write!(f, "has a Genesis ROM header (\"SEGA\" at offset 0x100)"),
            Self::NativeBinary => write!(
                f,
                "is a native program or library (emulators and other tools run from outside \
                 the repository)"
            ),
            Self::DebuggerScript => write!(
                f,
                "is a reference emulator debugger script (generate it outside the repository)"
            ),
            Self::GamesFolder => write!(
                f,
                "is in {GAMES_FOLDER}, where per-game configs lived (games build from the ROM \
                 alone)"
            ),
            Self::HintSection { name } => write!(
                f,
                "holds a [[{name}]] hint section (games build from the ROM alone)"
            ),
        }
    }
}

/// Checks every tracked or addable file and reports each violation on stderr.
///
/// Untracked files that are not gitignored are checked too, so a ROM copied outside
/// `roms/` fails before it is ever staged.
///
/// # Errors
/// Returns an error if git cannot list the files or a listed file cannot be read.
pub(crate) fn run() -> Result<ExitCode> {
    let root = workspace_root()?;
    let files = candidate_files(&root)?;
    let mut failed = false;

    for file in &files {
        // TOML files are read whole for the hint check; the size rule keeps them small.
        let limit = if is_toml(Path::new(file)) {
            MAX_FILE_SIZE
        } else {
            HEAD_LEN as u64
        };
        let Some((size, head)) =
            read_head(&root.join(file), limit).with_context(|| format!("reading {file}"))?
        else {
            eprintln!("warning: {file} is tracked but missing from the working tree; skipped");
            continue;
        };
        for violation in check_file(Path::new(file), size, &head) {
            eprintln!("{file} {violation}");
            failed = true;
        }
    }

    if failed {
        eprintln!(
            "legal guard failed: ROM images, ROM-derived data and external tools never enter \
             the repository"
        );
        return Ok(ExitCode::FAILURE);
    }
    println!("legal guard passed: {} files checked", files.len());
    Ok(ExitCode::SUCCESS)
}

/// Returns every rule broken by a file with this path, size and leading bytes (all of them
/// for a TOML file).
fn check_file(path: &Path, size: u64, head: &[u8]) -> Vec<Violation> {
    let mut violations = Vec::new();
    if size > MAX_FILE_SIZE {
        violations.push(Violation::TooLarge { size });
    }
    if has_extension(path, &ROM_EXTENSIONS) {
        violations.push(Violation::RomExtension);
    }
    if head.get(HEADER_OFFSET..HEAD_LEN) == Some(HEADER_MAGIC) {
        violations.push(Violation::RomHeader);
    }
    if has_extension(path, &NATIVE_EXTENSIONS)
        || NATIVE_MAGICS.iter().any(|magic| head.starts_with(magic))
    {
        violations.push(Violation::NativeBinary);
    }
    if head.starts_with(SCRIPT_MAGIC) {
        violations.push(Violation::DebuggerScript);
    }
    if path.starts_with(GAMES_FOLDER) {
        violations.push(Violation::GamesFolder);
    }
    if is_toml(path) {
        violations.extend(hint_sections(head).map(|name| Violation::HintSection { name }));
    }
    violations
}

fn is_toml(path: &Path) -> bool {
    has_extension(path, &["toml"])
}

/// The hint section names `text` opens as TOML arrays of tables, in order, each once.
fn hint_sections(text: &[u8]) -> impl Iterator<Item = &'static str> {
    let text = String::from_utf8_lossy(text);
    let mut found = Vec::new();
    for line in text.lines() {
        let Some(name) = line
            .trim_start()
            .strip_prefix("[[")
            .and_then(|rest| rest.split_once("]]"))
            .map(|(name, _)| name.trim())
        else {
            continue;
        };
        if let Some(&hint) = HINT_SECTIONS.iter().find(|&&hint| hint == name)
            && !found.contains(&hint)
        {
            found.push(hint);
        }
    }
    found.into_iter()
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        extensions
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

fn candidate_files(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .context("running git ls-files")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_file_list(&output.stdout)
}

/// Splits `git ls-files -z` output into paths.
fn parse_file_list(stdout: &[u8]) -> Result<Vec<String>> {
    let list = std::str::from_utf8(stdout).context("git ls-files printed a non-UTF-8 path")?;
    Ok(list
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Returns the file size and up to `limit` leading bytes, or `None` if the file does not
/// exist.
fn read_head(path: &Path, limit: u64) -> io::Result<Option<(u64, Vec<u8>)>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();
    let mut head = Vec::new();
    file.take(limit).read_to_end(&mut head)?;
    Ok(Some((size, head)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic cartridge image: zero-filled, with `magic` written at the header offset.
    fn fixture_rom(magic: &[u8]) -> Vec<u8> {
        let mut image = vec![0; 0x200];
        image[HEADER_OFFSET..HEADER_OFFSET + magic.len()].copy_from_slice(magic);
        image
    }

    fn check(path: &str, contents: &[u8]) -> Vec<Violation> {
        check_file(Path::new(path), contents.len() as u64, contents)
    }

    #[test]
    fn test_check_file_synthetic_rom_header_rejected() {
        let image = fixture_rom(b"SEGA MEGA DRIVE ");
        assert_eq!(check("data/image.dat", &image), [Violation::RomHeader]);
    }

    #[test]
    fn test_check_file_rom_header_with_markdown_extension_rejected() {
        let image = fixture_rom(b"SEGA GENESIS    ");
        assert_eq!(check("sonic.md", &image), [Violation::RomHeader]);
    }

    #[test]
    fn test_check_file_rom_extensions_any_case_rejected() {
        for path in ["roms/game.bin", "game.GEN", "dump.Smd"] {
            assert_eq!(check(path, b"tiny"), [Violation::RomExtension], "{path}");
        }
    }

    #[test]
    fn test_check_file_over_size_limit_rejected() {
        let size = MAX_FILE_SIZE + 1;
        let violations = check_file(Path::new("assets/blob.dat"), size, b"");
        assert_eq!(violations, [Violation::TooLarge { size }]);
    }

    #[test]
    fn test_check_file_at_size_limit_accepted() {
        assert!(check_file(Path::new("assets/blob.dat"), MAX_FILE_SIZE, b"").is_empty());
    }

    #[test]
    fn test_check_file_rom_shaped_bin_reports_every_violation() {
        let image = fixture_rom(b"SEGA");
        let violations = check_file(Path::new("sonic.bin"), 512 * 1024 * 3, &image);
        assert_eq!(
            violations,
            [
                Violation::TooLarge {
                    size: 512 * 1024 * 3
                },
                Violation::RomExtension,
                Violation::RomHeader,
            ]
        );
    }

    #[test]
    fn test_check_file_normal_sources_accepted() {
        assert!(check("xtask/src/main.rs", b"fn main() {}\n").is_empty());
        assert!(check("README.md", b"# AnyGenesis\n").is_empty());
        assert!(check("crates/rom/Cargo.toml", b"[package]\nname = \"rom\"\n").is_empty());
    }

    #[test]
    fn test_check_file_games_folder_rejected() {
        for path in ["games/sonic1.toml", "games/notes.md"] {
            assert_eq!(check(path, b"tiny"), [Violation::GamesFolder], "{path}");
        }
        assert!(check("crates/games/src/lib.rs", b"//! Games.\n").is_empty());
    }

    #[test]
    fn test_check_file_hint_sections_in_toml_rejected() {
        let text = b"# hints\nsha1 = \"\"\n\n[[jump_table]]\naddress = 0x396\n\
                     [[ indirect ]]\nsite = 0x300\n[[jump_table]]\naddress = 0x400\n";
        assert_eq!(
            check("crates/game/tests/sonic1.toml", text),
            [
                Violation::HintSection { name: "jump_table" },
                Violation::HintSection { name: "indirect" },
            ]
        );
    }

    #[test]
    fn test_check_file_other_toml_sections_accepted() {
        let masks = b"[[mask]]\nregion = \"ram\"\nstart = 0xFFFDF3\nend = 0xFFFDF4\n";
        assert!(check("crates/framediff/tests/masks/sonic1.toml", masks).is_empty());
        let manifest = b"[[bin]]\nname = \"recomp\"\n[dependencies]\n";
        assert!(check("crates/recomp/Cargo.toml", manifest).is_empty());
        let prose = b"The old [[jump_table]] hints are gone.\n";
        assert!(check("notes.md", prose).is_empty());
    }

    #[test]
    fn test_check_file_native_programs_rejected() {
        for path in ["tools/blastem.exe", "SDL2.DLL", "lib/libSDL2.so", "x.dylib"] {
            assert_eq!(check(path, b"tiny"), [Violation::NativeBinary], "{path}");
        }
        assert_eq!(
            check("tools/blastem", b"\x7FELF\x02\x01"),
            [Violation::NativeBinary]
        );
        assert_eq!(check("tools/run", b"MZ\x90\x00"), [Violation::NativeBinary]);
    }

    #[test]
    fn test_check_file_debugger_script_rejected() {
        let script = b"breakpoint $B10\ncontinue\nsave \"00001-ram.bin\" [$FF0000] $10000\n";
        assert_eq!(check("dumps.txt", script), [Violation::DebuggerScript]);
        assert!(check("notes.md", b"A breakpoint $B10 line in prose\n").is_empty());
    }

    #[test]
    fn test_check_file_magic_elsewhere_accepted() {
        let mut image = fixture_rom(b"");
        image[..4].copy_from_slice(b"SEGA");
        image[HEADER_OFFSET + 1..HEADER_OFFSET + 5].copy_from_slice(b"SEGA");
        assert!(check("data/image.dat", &image).is_empty());
    }

    #[test]
    fn test_check_file_truncated_header_accepted() {
        let image = fixture_rom(b"SEGA");
        assert!(check("data/image.dat", &image[..HEAD_LEN - 1]).is_empty());
    }

    #[test]
    fn test_parse_file_list_nul_separated_returns_paths() {
        let paths = parse_file_list(b"Cargo.toml\0crates/rom/src/lib.rs\0dir with space/a.md\0");
        assert_eq!(
            paths.ok(),
            Some(vec![
                "Cargo.toml".to_owned(),
                "crates/rom/src/lib.rs".to_owned(),
                "dir with space/a.md".to_owned(),
            ])
        );
    }
}
