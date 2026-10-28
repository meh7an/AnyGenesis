//! The system C toolchain a game program is compiled and linked with: clang-cl and the
//! MSVC linker from a Visual Studio installation on Windows, clang on PATH elsewhere.
//! Generated code needs clang: cross-function jumps rely on `musttail`.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

/// The Rust target the player library is built for on Windows.
#[cfg(windows)]
const TARGET: &str = "x86_64-pc-windows-msvc";

/// The system libraries the player library needs, as rustc's `--print native-static-libs`
/// lists them for it (SDL3, the Rust standard library); the C runtime comes from the
/// library's own default-library directives.
#[cfg(windows)]
const SYSTEM_LIBRARIES: [&str; 18] = [
    "kernel32.lib",
    "user32.lib",
    "gdi32.lib",
    "winmm.lib",
    "imm32.lib",
    "ole32.lib",
    "oleaut32.lib",
    "version.lib",
    "uuid.lib",
    "advapi32.lib",
    "setupapi.lib",
    "shell32.lib",
    "dinput8.lib",
    "legacy_stdio_definitions.lib",
    "ntdll.lib",
    "userenv.lib",
    "ws2_32.lib",
    "dbghelp.lib",
];
#[cfg(not(windows))]
const SYSTEM_LIBRARIES: [&str; 7] = [
    "-lm",
    "-lgcc_s",
    "-lutil",
    "-lrt",
    "-lpthread",
    "-ldl",
    "-lc",
];

/// The file name of the player library next to anygenesis.
#[cfg(windows)]
pub const PLAYER_LIBRARY: &str = "anygenesis_player_lib.lib";
#[cfg(not(windows))]
pub const PLAYER_LIBRARY: &str = "libanygenesis_player_lib.a";

/// The file name extension of a native program.
pub const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

/// The C compiler, found once, with the environment it runs in.
#[derive(Debug)]
pub struct Toolchain {
    compiler: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl Toolchain {
    /// Finds clang-cl in the Visual Studio installation (its "C++ Clang tools for Windows"
    /// component).
    ///
    /// # Errors
    /// Says what to install when there is none.
    #[cfg(windows)]
    pub fn find() -> Result<Self> {
        let Some(tool) = cc::windows_registry::find_tool(TARGET, "clang-cl.exe") else {
            bail!(
                "clang-cl was not found: anygenesis compiles games with the clang-cl of a \
                 Visual Studio installation; install Visual Studio (or its Build Tools) with \
                 the \"C++ Clang tools for Windows\" component"
            );
        };
        Ok(Self {
            compiler: tool.path().to_owned(),
            env: tool
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        })
    }

    /// Finds clang on PATH.
    ///
    /// # Errors
    /// Says what to install when there is none.
    #[cfg(not(windows))]
    pub fn find() -> Result<Self> {
        let found = Command::new("clang")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !found {
            bail!(
                "clang was not found on PATH: anygenesis compiles games with clang; install \
                 your distribution's clang package"
            );
        }
        Ok(Self {
            compiler: PathBuf::from("clang"),
            env: Vec::new(),
        })
    }

    /// Where the compiler is.
    pub fn compiler(&self) -> &Path {
        &self.compiler
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.compiler);
        command.envs(self.env.iter().map(|(key, value)| (key, value)));
        command
    }

    /// Compiles `source` into `object` with the flags cc gives a game build: C11, warnings
    /// as errors, the DLL C runtime on Windows, and at the size-oriented level for a stepped
    /// file (/O1 on Windows, -O1 elsewhere) or the release level otherwise.
    #[cfg(windows)]
    pub fn compile(&self, source: &Path, object: &Path, stepped: bool) -> Command {
        let mut command = self.command();
        command
            .args(["-nologo", "-MD", if stepped { "-O1" } else { "-O2" }])
            .args(["-Brepro", &format!("--target={TARGET}")])
            .args(["-std:c11", "-W4", "-WX", "-c"])
            .arg(source)
            .arg(prefixed("-Fo", object));
        command
    }

    /// Compiles `source` into `object`; see the Windows version.
    #[cfg(not(windows))]
    pub fn compile(&self, source: &Path, object: &Path, stepped: bool) -> Command {
        let mut command = self.command();
        command
            .arg(if stepped { "-O1" } else { "-O3" })
            .args(["-ffunction-sections", "-fdata-sections", "-fPIC", "-m64"])
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-c"])
            .arg(source)
            .arg("-o")
            .arg(object);
        command
    }

    /// Links `objects` with the player library `player` into the stripped program `exe`,
    /// through the response file `arguments` (the object list outgrows Windows' command
    /// line), with the MSVC linker and without debug information.
    #[cfg(windows)]
    pub fn link(
        &self,
        objects: &[PathBuf],
        player: &Path,
        exe: &Path,
        arguments: &Path,
    ) -> Result<Command> {
        let mut lines: Vec<String> = vec!["-nologo".to_owned(), "-fuse-ld=link".to_owned()];
        lines.extend(objects.iter().map(|object| quoted(object)));
        lines.push(quoted(player));
        lines.extend(SYSTEM_LIBRARIES.iter().map(|library| (*library).to_owned()));
        lines.push(format!("-Fe{}", quoted(exe)));
        lines.extend(["-link", "-subsystem:console"].map(str::to_owned));
        write_arguments(arguments, &lines)?;
        let mut command = self.command();
        command.arg(prefixed("@", arguments));
        Ok(command)
    }

    /// Links `objects` with `player` into the stripped program `exe`; see the Windows
    /// version.
    #[cfg(not(windows))]
    pub fn link(
        &self,
        objects: &[PathBuf],
        player: &Path,
        exe: &Path,
        arguments: &Path,
    ) -> Result<Command> {
        let mut lines: Vec<String> = vec!["-s".to_owned(), "-o".to_owned(), quoted(exe)];
        lines.extend(objects.iter().map(|object| quoted(object)));
        lines.push(quoted(player));
        lines.extend(SYSTEM_LIBRARIES.iter().map(|library| (*library).to_owned()));
        write_arguments(arguments, &lines)?;
        let mut command = self.command();
        command.arg(prefixed("@", arguments));
        Ok(command)
    }
}

/// `prefix` followed by `path`, as one argument.
fn prefixed(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

/// `path` in double quotes for a clang response file, which reads backslashes literally
/// except before a quote; paths hold no quotes.
fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

fn write_arguments(path: &Path, lines: &[String]) -> Result<()> {
    std::fs::write(path, lines.join("\n") + "\n")
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quoted_wraps_paths_with_spaces_in_one_argument() {
        assert_eq!(quoted(Path::new("a b/c.o")), "\"a b/c.o\"");
    }

    #[test]
    fn test_compile_puts_stepped_files_at_the_size_level() {
        let toolchain = Toolchain {
            compiler: PathBuf::from("clang"),
            env: Vec::new(),
        };
        let arguments = |stepped| -> Vec<String> {
            toolchain
                .compile(Path::new("ag_s000.c"), Path::new("ag_s000.o"), stepped)
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect()
        };
        assert!(arguments(true).iter().any(|argument| argument == "-O1"));
        assert!(!arguments(false).iter().any(|argument| argument == "-O1"));
    }
}
