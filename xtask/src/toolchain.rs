//! Builds what a user builds games with, for this host's target, into
//! target/bundle/<host>/: anygenesis and the builder app anygenesis-gui, built to need no Rust
//! or C installation, and next to them toolchain/ with the pinned Zig pruned to what a game build needs (zig/), the player
//! library built with that Zig, SDL3 inside it (lib/), and toolchain.txt naming both.
//! anygenesis reads that layout (its toolchain.rs). Nothing here is ever committed: Zig and
//! the libraries are native binaries.

use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anygenesis_patch::sha256;
use anyhow::{Context, Result, bail};

use crate::workspace_root;

/// The pinned Zig. Move it on purpose: the archives below and anygenesis's cache key follow.
const ZIG_VERSION: &str = "0.17.0";

/// What Zig needs from its own folder for any C build: the compiler's headers, the sources it
/// builds compiler-rt and libunwind from, and the standard library its build steps run on.
const ZIG_KEEP: [&str; 12] = [
    "LICENSE",
    "lib/std",
    "lib/include",
    "lib/compiler",
    "lib/compiler_rt",
    "lib/libunwind",
    "lib/c",
    "lib/c.zig",
    "lib/compiler_rt.zig",
    "lib/fuzzer.zig",
    "lib/ubsan_rt.zig",
    "lib/zig.h",
];

/// The player library's file name, in the cargo output and in toolchain/lib/.
const PLAYER_LIBRARY: &str = "libanygenesis_player_lib.a";

/// One host's toolchain.
pub(crate) struct Host {
    /// The bundle's folder under target/bundle/.
    pub(crate) name: &'static str,
    /// The pinned Zig archive's file name, its SHA-256 (ziglang.org/download/index.json) and
    /// the folder it unpacks into.
    archive: &'static str,
    sha256: &'static str,
    folder: &'static str,
    /// What Zig builds games for.
    zig_target: &'static str,
    /// What Rust builds the player library and anygenesis for.
    pub(crate) player_target: &'static str,
    pub(crate) tool_target: &'static str,
    /// The C library sources and headers of `zig_target`, on top of `ZIG_KEEP`.
    libc: &'static [&'static str],
    /// The licence text of those C library files, under Zig's folder.
    pub(crate) libc_notice: &'static str,
}

#[cfg(windows)]
pub(crate) const HOST: Host = Host {
    name: "windows-x64",
    archive: "zig-x86_64-windows-0.17.0.zip",
    sha256: "b5663f69581dcf391293fbf16c06cb80d81d806545ce618b4d0bab7f0eb8c428",
    folder: "zig-x86_64-windows-0.17.0",
    zig_target: "x86_64-windows-gnu",
    player_target: "x86_64-pc-windows-gnullvm",
    // The static C runtime, so anygenesis needs no Visual C++ runtime (see `tool_settings`).
    tool_target: "x86_64-pc-windows-msvc",
    libc: &["lib/libc/mingw", "lib/libc/include/any-windows-any"],
    libc_notice: "lib/libc/mingw/COPYING",
};

#[cfg(not(windows))]
pub(crate) const HOST: Host = Host {
    name: "linux-x64",
    archive: "zig-x86_64-linux-0.17.0.tar.xz",
    sha256: "1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026",
    folder: "zig-x86_64-linux-0.17.0",
    // The oldest glibc a game (and anygenesis) needs.
    zig_target: "x86_64-linux-gnu.2.28",
    player_target: "x86_64-unknown-linux-gnu",
    tool_target: "x86_64-unknown-linux-gnu",
    libc: &[
        "lib/libc/glibc",
        "lib/libc/include/any-linux-any",
        "lib/libc/include/generic-glibc",
        "lib/libc/include/x86-linux-any",
        "lib/libc/include/x86-linux-gnu",
    ],
    libc_notice: "lib/libc/glibc/LICENSES",
};

const ZIG_EXE: &str = if cfg!(windows) { "zig.exe" } else { "zig" };

/// Builds the bundle from the pinned Zig archive at `archive` (a local download).
///
/// # Errors
/// No archive given, an archive other than the pinned one, or a step that fails.
pub(crate) fn run(archive: Option<&str>) -> Result<ExitCode> {
    let Some(archive) = archive.map(PathBuf::from) else {
        bail!(
            "usage: cargo xtask toolchain <path to {}> (Zig {ZIG_VERSION} from ziglang.org)",
            HOST.archive
        );
    };
    let root = workspace_root()?;
    let work = root.join("target").join("toolchain-work");
    let zig_root = unpack(&archive, &work)?;
    let wrappers = Wrappers::write(&zig_root.join(ZIG_EXE), &work.join("wrappers"))?;
    let player = build_player(&root, &work, &wrappers)?;
    let tools = build_tools(&root, &work, &wrappers)?;
    let bundle = root.join("target").join("bundle").join(HOST.name);
    let zig_bytes = assemble(&bundle, &zig_root, &player, &tools, &player_commit(&root)?)?;
    println!(
        "bundle written to {}: Zig {ZIG_VERSION} pruned to {:.1} MB, player library {:.1} MB",
        bundle.display(),
        megabytes(zig_bytes),
        megabytes(file_size(&player)?)
    );
    Ok(ExitCode::SUCCESS)
}

/// Checks `archive` against the pin and unpacks it into `work`, returning Zig's folder.
fn unpack(archive: &Path, work: &Path) -> Result<PathBuf> {
    if archive.file_name() != Some(OsStr::new(HOST.archive)) {
        bail!(
            "{} is not the pinned Zig archive {}",
            archive.display(),
            HOST.archive
        );
    }
    let bytes = fs::read(archive).with_context(|| format!("reading {}", archive.display()))?;
    let digest = sha256::hex_digest(&bytes);
    if digest != HOST.sha256 {
        bail!(
            "{} has SHA-256 {digest}, not the pinned {}",
            archive.display(),
            HOST.sha256
        );
    }
    let unpacked = work.join("zig");
    remove_folder(&unpacked)?;
    fs::create_dir_all(&unpacked).with_context(|| format!("creating {}", unpacked.display()))?;
    let mut tar = Command::new(tar_program());
    tar.arg("-xf").arg(archive).arg("-C").arg(&unpacked);
    run_tool(&mut tar, "unpacking Zig")?;
    let zig_root = unpacked.join(HOST.folder);
    if !zig_root.join(ZIG_EXE).is_file() {
        bail!("{} holds no {ZIG_EXE} after unpacking", zig_root.display());
    }
    Ok(zig_root)
}

/// The bsdtar every Windows 10 and later ships, which also unpacks zip files; a tar from
/// Git or MSYS earlier on PATH may not.
#[cfg(windows)]
pub(crate) fn tar_program() -> PathBuf {
    let system = env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&system).join("System32").join("tar.exe")
}

#[cfg(not(windows))]
pub(crate) fn tar_program() -> PathBuf {
    PathBuf::from("tar")
}

/// Scripts that run one `zig` subcommand for the target, so cc-rs, CMake and rustc can take
/// each as a single program.
#[derive(Debug)]
struct Wrappers {
    cc: PathBuf,
    cxx: PathBuf,
    ar: PathBuf,
    ranlib: PathBuf,
    /// The CMake toolchain file SDL3's build reads; without it CMake ignores CC and picks
    /// whatever compiler is on PATH.
    cmake: PathBuf,
}

impl Wrappers {
    fn write(zig: &Path, folder: &Path) -> Result<Self> {
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
        let target = format!("-target {}", HOST.zig_target);
        let script = |name: &str, command: &str| write_script(zig, folder, name, command);
        let wrappers = Self {
            cc: script("zigcc", &format!("cc {target}"))?,
            cxx: script("zigcxx", &format!("c++ {target}"))?,
            ar: script("zigar", "ar")?,
            ranlib: script("zigranlib", "ranlib")?,
            cmake: folder.join("toolchain.cmake"),
        };
        let mut lines = vec![
            format!("set(CMAKE_SYSTEM_NAME {})", cmake_system()),
            format!("set(CMAKE_C_COMPILER \"{}\")", cmake_path(&wrappers.cc)),
            format!("set(CMAKE_CXX_COMPILER \"{}\")", cmake_path(&wrappers.cxx)),
            format!(
                "set(CMAKE_AR \"{}\" CACHE FILEPATH \"\" FORCE)",
                cmake_path(&wrappers.ar)
            ),
            format!(
                "set(CMAKE_RANLIB \"{}\" CACHE FILEPATH \"\" FORCE)",
                cmake_path(&wrappers.ranlib)
            ),
            "set(SDL_SHARED OFF CACHE BOOL \"\" FORCE)".to_owned(),
            "set(SDL_TEST_LIBRARY OFF CACHE BOOL \"\" FORCE)".to_owned(),
        ];
        if cfg!(windows) {
            let rc = script("zigrc", "rc")?;
            lines.push(format!("set(CMAKE_RC_COMPILER \"{}\")", cmake_path(&rc)));
        }
        fs::write(&wrappers.cmake, lines.join("\n") + "\n")
            .with_context(|| format!("writing {}", wrappers.cmake.display()))?;
        Ok(wrappers)
    }
}

#[cfg(windows)]
fn write_script(zig: &Path, folder: &Path, name: &str, command: &str) -> Result<PathBuf> {
    let path = folder.join(format!("{name}.cmd"));
    let text = format!("@echo off\r\n\"{}\" {command} %*\r\n", zig.display());
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(not(windows))]
fn write_script(zig: &Path, folder: &Path, name: &str, command: &str) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = folder.join(name);
    let text = format!("#!/bin/sh\nexec \"{}\" {command} \"$@\"\n", zig.display());
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", path.display()))?;
    Ok(path)
}

fn cmake_system() -> &'static str {
    if cfg!(windows) { "Windows" } else { "Linux" }
}

/// `path` with forward slashes, which CMake reads on every system.
fn cmake_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Builds the player library with Zig as its C compiler (SDL3 included) in a cargo command
/// of its own, so it never shares anygenesis's runtime features (the interpreter).
fn build_player(root: &Path, work: &Path, wrappers: &Wrappers) -> Result<PathBuf> {
    let target_dir = work.join("cargo-player");
    let key = HOST.player_target.replace('-', "_");
    let mut command = cargo(
        root,
        &["anygenesis-player-lib"],
        HOST.player_target,
        &target_dir,
    );
    command
        .env(format!("CC_{key}"), &wrappers.cc)
        .env(format!("CXX_{key}"), &wrappers.cxx)
        .env(format!("AR_{key}"), &wrappers.ar)
        .env(format!("RANLIB_{key}"), &wrappers.ranlib)
        .env(format!("CMAKE_TOOLCHAIN_FILE_{key}"), &wrappers.cmake)
        // cc-rs's own target flags are clang's, which `zig cc -target` refuses.
        .env("CRATE_CC_NO_DEFAULTS", "1")
        .env("CMAKE_GENERATOR", "Ninja");
    run_tool(&mut command, "building the player library")?;
    Ok(target_dir
        .join(HOST.player_target)
        .join("release")
        .join(PLAYER_LIBRARY))
}

/// The programs a user runs, each the package of the same name.
const TOOLS: [&str; 2] = ["anygenesis", "anygenesis-gui"];

/// Builds the `TOOLS` to run with nothing installed (see `tool_settings`) and returns their
/// paths.
fn build_tools(root: &Path, work: &Path, wrappers: &Wrappers) -> Result<Vec<PathBuf>> {
    let target_dir = work.join("cargo-tool");
    let mut command = cargo(root, &TOOLS, HOST.tool_target, &target_dir);
    tool_settings(&mut command, work, wrappers)?;
    run_tool(&mut command, "building anygenesis and anygenesis-gui")?;
    let release = target_dir.join(HOST.tool_target).join("release");
    Ok(TOOLS
        .iter()
        .map(|tool| release.join(format!("{tool}{}", env::consts::EXE_SUFFIX)))
        .collect())
}

/// The static C runtime, so the programs import only system DLLs (no Visual C++ runtime).
#[cfg(windows)]
fn tool_settings(command: &mut Command, _work: &Path, _wrappers: &Wrappers) -> Result<()> {
    command.env("RUSTFLAGS", "-Ctarget-feature=+crt-static");
    Ok(())
}

/// The desktop libraries anygenesis-gui links (GPUI's X11 keyboard and text), which every
/// desktop Linux has and the user's own copies of are loaded at run time.
#[cfg(not(windows))]
const GUI_SYSTEM_LIBRARIES: [&str; 4] = ["xcb", "xkbcommon", "xkbcommon-x11", "freetype"];

/// Zig as the linker, against its glibc 2.28 stubs, so the programs run on any glibc since.
/// Zig searches no system folder, so a folder of links to just `GUI_SYSTEM_LIBRARIES` is
/// passed to the link; a whole system folder would also offer the host's libc in place of
/// Zig's.
#[cfg(not(windows))]
fn tool_settings(command: &mut Command, work: &Path, wrappers: &Wrappers) -> Result<()> {
    let links = work.join("system-libraries");
    remove_folder(&links)?;
    fs::create_dir_all(&links).with_context(|| format!("creating {}", links.display()))?;
    for library in GUI_SYSTEM_LIBRARIES {
        let name = format!("lib{library}.so");
        let found = system_library(&name)
            .with_context(|| format!("{name} not found: install the {library} package"))?;
        std::os::unix::fs::symlink(&found, links.join(&name))
            .with_context(|| format!("linking {}", found.display()))?;
    }
    // The host's copies need a newer glibc than Zig's 2.28 stubs offer; those are their own
    // needs, met at run time by the user's glibc, so the link must not check them.
    let flags = format!(
        "-Lnative={} -Clink-arg=-Wl,--allow-shlib-undefined",
        links.display()
    );
    command
        .env("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", &wrappers.cc)
        .env("RUSTFLAGS", flags);
    Ok(())
}

/// The shared library `name` (`libx.so`) in a system folder: the development link if there is
/// one, else the runtime file `name.<major>` (a desktop has those without the -dev packages).
#[cfg(not(windows))]
fn system_library(name: &str) -> Option<PathBuf> {
    let folders = ["/usr/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib"];
    let link = folders.iter().map(|folder| Path::new(folder).join(name));
    let runtime = folders.iter().filter_map(|folder| {
        fs::read_dir(folder).ok()?.find_map(|entry| {
            let path = entry.ok()?.path();
            let file = path.file_name()?.to_str()?;
            let major = file.strip_prefix(name)?.strip_prefix('.')?;
            major
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then_some(path)
        })
    });
    link.filter(|path| path.exists()).chain(runtime).next()
}

/// `cargo build --release --locked -p <package>... --target <target>` into `target_dir`.
fn cargo(root: &Path, packages: &[&str], target: &str, target_dir: &Path) -> Command {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(root)
        .args(["build", "--release", "--locked"])
        .args(packages.iter().flat_map(|package| ["-p", package]))
        .args(["--target", target])
        .env("CARGO_TARGET_DIR", target_dir);
    command
}

/// Writes the bundle afresh and returns the pruned Zig's size in bytes.
fn assemble(
    bundle: &Path,
    zig_root: &Path,
    player: &Path,
    tools: &[PathBuf],
    commit: &str,
) -> Result<u64> {
    remove_folder(bundle)?;
    let toolchain = bundle.join("toolchain");
    let zig = toolchain.join("zig");
    let mut zig_bytes = 0;
    for entry in [ZIG_EXE].iter().chain(&ZIG_KEEP).chain(HOST.libc) {
        zig_bytes += copy_tree(&zig_root.join(entry), &zig.join(entry))?;
    }
    let lib = toolchain.join("lib");
    fs::create_dir_all(&lib).with_context(|| format!("creating {}", lib.display()))?;
    copy_tree(player, &lib.join(PLAYER_LIBRARY))?;
    for tool in tools {
        let name = tool
            .file_name()
            .context("a built program has no file name")?;
        copy_tree(tool, &bundle.join(name))?;
    }
    let manifest = toolchain.join("toolchain.txt");
    fs::write(&manifest, manifest_text(commit))
        .with_context(|| format!("writing {}", manifest.display()))?;
    Ok(zig_bytes)
}

/// toolchain.txt: one `key value` line each for the Zig version and archive digest, the
/// target games are built for, the commit the player library was built from and the release.
fn manifest_text(commit: &str) -> String {
    format!(
        "zig {ZIG_VERSION}\nzig-sha256 {}\ntarget {}\nplayer {commit}\nanygenesis {}\n",
        HOST.sha256,
        HOST.zig_target,
        env!("CARGO_PKG_VERSION")
    )
}

/// The checked-out commit, with `-dirty` when the working tree has changes.
pub(crate) fn player_commit(root: &Path) -> Result<String> {
    let git = |arguments: &[&str]| -> Result<String> {
        let output = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .context("running git")?;
        if !output.status.success() {
            bail!("git {} failed", arguments.join(" "));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "HEAD"])?;
    let dirty = !git(&["status", "--porcelain"])?.is_empty();
    Ok(if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    })
}

/// Copies the file or folder `from` to `to` and returns the bytes copied.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<u64> {
    if from.is_dir() {
        fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
        let mut bytes = 0;
        for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
            let entry = entry.with_context(|| format!("reading {}", from.display()))?;
            bytes += copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(bytes);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::copy(from, to).with_context(|| format!("copying {} to {}", from.display(), to.display()))
}

pub(crate) fn run_tool(command: &mut Command, what: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("{what}: running {}", command.get_program().display()))?;
    if !status.success() {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

pub(crate) fn remove_folder(folder: &Path) -> Result<()> {
    match fs::remove_dir_all(folder) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", folder.display())),
    }
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len())
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_text_names_zig_target_and_commit_one_per_line() {
        let text = manifest_text("abc123");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("zig {ZIG_VERSION}"));
        assert_eq!(lines[1], format!("zig-sha256 {}", HOST.sha256));
        assert_eq!(lines[2], format!("target {}", HOST.zig_target));
        assert_eq!(lines[3], "player abc123");
        assert_eq!(
            lines[4],
            format!("anygenesis {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn test_host_archive_is_the_pinned_version() {
        assert!(HOST.archive.contains(ZIG_VERSION));
        assert!(HOST.folder.contains(ZIG_VERSION));
        assert_eq!(HOST.sha256.len(), 64);
    }
}
