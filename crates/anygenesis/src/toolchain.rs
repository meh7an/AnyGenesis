//! The toolchain a game program is compiled and linked with: the pinned Zig (clang, lld and
//! the C library for the target), or on macOS the clang of Apple's Command Line Tools, and
//! the prebuilt player library, in the toolchain folder next to anygenesis
//! (`ANYGENESIS_TOOLCHAIN` names another) as `cargo xtask toolchain` lays it out: zig/ (not
//! on macOS), lib/ and toolchain.txt. Generated code needs clang: cross-function jumps rely
//! on `musttail`.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

/// Names the toolchain folder instead of the one next to anygenesis.
const TOOLCHAIN_VAR: &str = "ANYGENESIS_TOOLCHAIN";

/// The toolchain folder's name next to anygenesis.
const TOOLCHAIN_FOLDER: &str = "toolchain";

/// The file in the toolchain folder naming its Zig version and target.
const MANIFEST: &str = "toolchain.txt";

/// What games are built for, as Zig (or on macOS clang) names it; the toolchain must be made
/// for the same.
#[cfg(windows)]
const TARGET: &str = "x86_64-windows-gnu";
#[cfg(target_os = "linux")]
const TARGET: &str = "x86_64-linux-gnu.2.28";
#[cfg(target_os = "macos")]
const TARGET: &str = "arm64-apple-macos11.0";

#[cfg(not(target_os = "macos"))]
const ZIG_EXE: &str = if cfg!(windows) { "zig.exe" } else { "zig" };

/// The clang of Apple's Command Line Tools (or Xcode), through the shim every Mac has: it
/// offers to install the tools when they are missing.
#[cfg(target_os = "macos")]
const APPLE_CLANG: &str = "/usr/bin/clang";

/// The player library's file name in the toolchain's lib/.
const PLAYER_LIBRARY: &str = "libanygenesis_player_lib.a";

/// The system libraries the player library needs, as rustc's `--print native-static-libs`
/// lists them for its target (SDL3, the Rust standard library); Zig brings every one, the
/// macOS SDK the frameworks.
#[cfg(windows)]
const SYSTEM_LIBRARIES: [&str; 19] = [
    "-lm",
    "-lkernel32",
    "-luser32",
    "-lgdi32",
    "-lwinmm",
    "-limm32",
    "-lole32",
    "-loleaut32",
    "-lversion",
    "-luuid",
    "-ladvapi32",
    "-lsetupapi",
    "-lshell32",
    "-ldinput8",
    "-lntdll",
    "-luserenv",
    "-lws2_32",
    "-ldbghelp",
    "-lunwind",
];
#[cfg(target_os = "linux")]
const SYSTEM_LIBRARIES: [&str; 7] = [
    "-lm",
    "-lgcc_s",
    "-lutil",
    "-lrt",
    "-lpthread",
    "-ldl",
    "-lc",
];
#[cfg(target_os = "macos")]
const SYSTEM_LIBRARIES: [&str; 34] = [
    "-framework",
    "CoreMedia",
    "-framework",
    "CoreVideo",
    "-framework",
    "Cocoa",
    "-framework",
    "UniformTypeIdentifiers",
    "-framework",
    "IOKit",
    "-framework",
    "ForceFeedback",
    "-framework",
    "Carbon",
    "-framework",
    "CoreAudio",
    "-framework",
    "AudioToolbox",
    "-framework",
    "AVFoundation",
    "-framework",
    "Foundation",
    "-framework",
    "GameController",
    "-framework",
    "Metal",
    "-framework",
    "QuartzCore",
    "-framework",
    "CoreHaptics",
    "-liconv",
    "-lSystem",
    "-lc",
    "-lm",
];

/// The optimisation level of a file that is not stepped code, and the code generation flags
/// besides it: on Linux and macOS the ones cc gives a release build.
#[cfg(windows)]
const RELEASE_FLAGS: [&str; 1] = ["-O2"];
#[cfg(not(windows))]
const RELEASE_FLAGS: [&str; 4] = ["-O3", "-ffunction-sections", "-fdata-sections", "-fPIC"];

/// How the link strips the program: Zig's lld drops every symbol; Apple's linker drops debug
/// and local symbols, and signs the result ad hoc itself, which a later `strip` would break.
#[cfg(not(target_os = "macos"))]
const STRIP_FLAGS: [&str; 1] = ["-s"];
#[cfg(target_os = "macos")]
const STRIP_FLAGS: [&str; 2] = ["-Wl,-S", "-Wl,-x"];

/// The file name extension of a native program.
pub const EXE_SUFFIX: &str = env::consts::EXE_SUFFIX;

/// A checked toolchain folder.
#[derive(Debug)]
pub struct Toolchain {
    /// Zig, or Apple's clang on macOS.
    compiler: PathBuf,
    player: PathBuf,
    /// The Zig version the folder names, or Apple clang's own.
    version: String,
    /// Zig's global cache, shared by every build: the C library, compiler-rt and libunwind
    /// it builds on the first link.
    #[cfg_attr(
        target_os = "macos",
        expect(dead_code, reason = "Apple's clang keeps no cache")
    )]
    cache: PathBuf,
}

impl Toolchain {
    /// The toolchain folder for the anygenesis at `tool`, with Zig's cache in `cache`.
    ///
    /// # Errors
    /// The folder is missing, incomplete or made for another target.
    pub fn find(tool: &Path, cache: &Path) -> Result<Self> {
        let folder = match env::var_os(TOOLCHAIN_VAR) {
            Some(folder) => PathBuf::from(folder),
            None => tool
                .parent()
                .context("anygenesis's program has no folder")?
                .join(TOOLCHAIN_FOLDER),
        };
        Self::open(&folder, cache).with_context(|| {
            format!(
                "the toolchain folder {} is not usable: keep the whole {TOOLCHAIN_FOLDER} folder \
                 that came with anygenesis next to it (or name one with {TOOLCHAIN_VAR})",
                folder.display()
            )
        })
    }

    fn open(folder: &Path, cache: &Path) -> Result<Self> {
        let manifest_path = folder.join(MANIFEST);
        let manifest = fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let target = field(&manifest, &manifest_path, "target")?;
        if target != TARGET {
            bail!("it builds games for {target}, this anygenesis for {TARGET}");
        }
        let player = folder.join("lib").join(PLAYER_LIBRARY);
        if !player.exists() {
            bail!("{} is missing", player.display());
        }
        let (compiler, version) = find_compiler(folder, &manifest, &manifest_path)?;
        Ok(Self {
            compiler,
            player,
            version,
            cache: cache.to_owned(),
        })
    }

    /// Where the compiler is: Zig, or Apple's clang on macOS.
    pub fn compiler(&self) -> &Path {
        &self.compiler
    }

    /// The compiler and its version, as a build reports it.
    pub fn compiler_name(&self) -> String {
        format!("{COMPILER_NAME} {}", self.version)
    }

    /// Where the player library is.
    pub fn player(&self) -> &Path {
        &self.player
    }

    /// The compiler's version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// `zig cc` for the target. Zig keeps a copy of every object it compiles in its local
    /// cache, `scratch`, which a build removes when done; the global one is the user's.
    #[cfg(not(target_os = "macos"))]
    fn cc(&self, scratch: &Path) -> Command {
        let mut command = Command::new(&self.compiler);
        command
            .args(["cc", "-target", TARGET])
            .env("ZIG_GLOBAL_CACHE_DIR", &self.cache)
            .env("ZIG_LOCAL_CACHE_DIR", scratch);
        command
    }

    /// Apple's clang for the target; it keeps no cache.
    #[cfg(target_os = "macos")]
    fn cc(&self, _scratch: &Path) -> Command {
        let mut command = Command::new(&self.compiler);
        command.args(["-target", TARGET]);
        command
    }

    /// Compiles `source` into `object` with Zig's local cache in `scratch`: C11, warnings as
    /// errors, no debug information (Zig emits it by default; it doubles the compile time and
    /// the link strips it), and at the size-oriented level for a stepped file (`-Os`: `zig cc`
    /// reads `-O1` as `-O2`) or the release level otherwise.
    pub fn compile(&self, source: &Path, object: &Path, stepped: bool, scratch: &Path) -> Command {
        let mut command = self.cc(scratch);
        if stepped {
            command.arg("-Os");
        } else {
            command.args(RELEASE_FLAGS);
        }
        command
            .args(["-g0", "-std=c11", "-Wall", "-Wextra", "-Werror", "-c"])
            .arg(source)
            .arg("-o")
            .arg(object);
        command
    }

    /// Links `objects` with the player library into the stripped program `exe`, through the
    /// response file `arguments` (the object list outgrows Windows' command line), with Zig's
    /// local cache in `scratch`.
    ///
    /// # Errors
    /// Writing the response file.
    pub fn link(
        &self,
        objects: &[PathBuf],
        exe: &Path,
        arguments: &Path,
        scratch: &Path,
    ) -> Result<Command> {
        let mut lines: Vec<String> = STRIP_FLAGS.iter().map(|flag| (*flag).to_owned()).collect();
        lines.extend(["-o".to_owned(), quoted(exe)]);
        lines.extend(objects.iter().map(|object| quoted(object)));
        lines.push(quoted(&self.player));
        lines.extend(SYSTEM_LIBRARIES.iter().map(|library| (*library).to_owned()));
        fs::write(arguments, lines.join("\n") + "\n")
            .with_context(|| format!("writing {}", arguments.display()))?;
        let mut command = self.cc(scratch);
        let mut argument = OsString::from("@");
        argument.push(arguments);
        command.arg(argument);
        Ok(command)
    }
}

/// The value of the `key value` line `key` in the manifest `text` read from `path`.
fn field<'a>(text: &'a str, path: &Path, key: &str) -> Result<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
        .with_context(|| format!("{} names no {key}", path.display()))
}

#[cfg(not(target_os = "macos"))]
const COMPILER_NAME: &str = "Zig";
#[cfg(target_os = "macos")]
const COMPILER_NAME: &str = "Apple clang";

/// Zig in the toolchain's zig/ and the version the manifest names. Zig finds its lib/ next to
/// its program; a folder still being copied may lack it.
#[cfg(not(target_os = "macos"))]
fn find_compiler(folder: &Path, manifest: &str, path: &Path) -> Result<(PathBuf, String)> {
    let zig_folder = folder.join("zig");
    let zig = zig_folder.join(ZIG_EXE);
    for needed in [&zig, &zig_folder.join("lib").join("std")] {
        if !needed.exists() {
            bail!("{} is missing", needed.display());
        }
    }
    Ok((zig, field(manifest, path, "zig")?.to_owned()))
}

/// Apple's clang and its version, from the first line of `clang --version`.
#[cfg(target_os = "macos")]
fn find_compiler(_folder: &Path, _manifest: &str, _path: &Path) -> Result<(PathBuf, String)> {
    let clang = PathBuf::from(APPLE_CLANG);
    let missing = || {
        format!(
            "{APPLE_CLANG} did not run: games are built with Apple's Command Line Tools, \
             installed with `xcode-select --install`"
        )
    };
    let output = Command::new(&clang)
        .arg("--version")
        .output()
        .with_context(missing)?;
    if !output.status.success() {
        bail!(missing());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Apple clang version "))
        .with_context(|| format!("{APPLE_CLANG} is not Apple's clang: {}", text.trim()))?;
    Ok((clang, version.to_owned()))
}

/// `path` in double quotes for a response file, with forward slashes: Windows takes them, and
/// a backslash may escape the next character there.
fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display().to_string().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let folder = env::temp_dir().join(format!(
            "anygenesis-toolchain-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&folder);
        folder
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("the folder is created");
        fs::write(path, text).expect("written");
    }

    #[test]
    fn test_quoted_wraps_paths_with_spaces_in_one_argument_with_forward_slashes() {
        assert_eq!(quoted(Path::new("a b/c.o")), "\"a b/c.o\"");
        assert_eq!(quoted(Path::new(r"C:\a b\c.o")), "\"C:/a b/c.o\"");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn test_open_complete_folder_reads_version_and_missing_parts_fail() {
        let root = folder("open");
        write(
            &root.join(MANIFEST),
            &format!("zig 0.17.0\nzig-sha256 00\ntarget {TARGET}\nplayer abc\n"),
        );
        assert!(Toolchain::open(&root, &root).is_err(), "no Zig yet");
        write(&root.join("zig").join(ZIG_EXE), "");
        write(&root.join("lib").join(PLAYER_LIBRARY), "");
        assert!(Toolchain::open(&root, &root).is_err(), "no lib/std yet");
        fs::create_dir_all(root.join("zig").join("lib").join("std")).expect("created");
        let toolchain = Toolchain::open(&root, &root).expect("complete");
        assert_eq!(toolchain.version(), "0.17.0");

        write(&root.join(MANIFEST), "zig 0.17.0\ntarget wasm32-wasi\n");
        assert!(Toolchain::open(&root, &root).is_err(), "another target");
        fs::remove_dir_all(&root).expect("the test folder is removed");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_open_complete_folder_finds_apple_clang_and_missing_parts_fail() {
        let root = folder("open");
        write(
            &root.join(MANIFEST),
            &format!("target {TARGET}\nplayer abc\n"),
        );
        assert!(
            Toolchain::open(&root, &root).is_err(),
            "no player library yet"
        );
        write(&root.join("lib").join(PLAYER_LIBRARY), "");
        let toolchain = Toolchain::open(&root, &root).expect("complete");
        assert_eq!(toolchain.compiler(), Path::new(APPLE_CLANG));
        assert!(toolchain.compiler_name().starts_with("Apple clang "));

        write(&root.join(MANIFEST), "target wasm32-wasi\n");
        assert!(Toolchain::open(&root, &root).is_err(), "another target");
        fs::remove_dir_all(&root).expect("the test folder is removed");
    }

    #[test]
    fn test_compile_puts_stepped_files_at_the_size_level() {
        let toolchain = Toolchain {
            compiler: PathBuf::from("zig"),
            player: PathBuf::from(PLAYER_LIBRARY),
            version: "0.17.0".to_owned(),
            cache: PathBuf::from("cache"),
        };
        let arguments = |stepped| -> Vec<String> {
            toolchain
                .compile(
                    Path::new("ag_s000.c"),
                    Path::new("ag_s000.o"),
                    stepped,
                    Path::new("scratch"),
                )
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect()
        };
        let stepped = arguments(true);
        let target = stepped
            .iter()
            .position(|argument| argument == "-target")
            .expect("the target is named");
        assert_eq!(stepped[target + 1..target + 3], [TARGET, "-Os"]);
        assert_eq!(arguments(false)[target + 2], RELEASE_FLAGS[0]);
    }
}
