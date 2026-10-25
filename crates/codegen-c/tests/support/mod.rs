//! Shared by the codegen tests: synthetic ROM images and compiling emitted C with clang.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use anygenesis_analysis::{Analysis, analyze};
use anygenesis_codegen_c::Output;
use anygenesis_gamecfg::GameConfig;
use anygenesis_rom::Rom;

const ROM_SIZE: usize = 0x1000;
/// Every exception vector points at an `rte` here.
pub const HANDLER: u32 = 0x200;
pub const RESET: u32 = 0x300;

/// A ROM image under construction.
pub struct Image {
    pub bytes: Vec<u8>,
}

impl Image {
    pub fn new() -> Self {
        let image = Self {
            bytes: vec![0; ROM_SIZE],
        };
        let image = (2..48).fold(image, |image, index| image.vector(index, HANDLER));
        image.vector(1, RESET).words(HANDLER, &[0x4E73])
    }

    pub fn vector(mut self, index: usize, value: u32) -> Self {
        self.bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        self
    }

    pub fn words(mut self, address: u32, words: &[u16]) -> Self {
        for (index, word) in words.iter().enumerate() {
            let offset = address as usize + index * 2;
            self.bytes[offset..offset + 2].copy_from_slice(&word.to_be_bytes());
        }
        self
    }

    /// Analyses the image with `hints` appended to its config.
    pub fn analyze(&self, hints: &str) -> Analysis {
        let rom = Rom::from_bytes(self.bytes.clone()).expect("synthetic image loads");
        let text = format!("sha1 = \"{}\"\n{hints}", rom.sha1());
        let config = GameConfig::parse(&text).expect("valid config");
        analyze(&rom, &config).expect("SHA-1 matches")
    }
}

/// Returns a branch opcode word followed by its 16-bit displacement to `target`.
pub fn branch_w(opcode: u16, site: u32, target: u32) -> [u16; 2] {
    [opcode, target.wrapping_sub(site + 2) as u16]
}

/// The optimization levels every emitted file is compiled at.
#[derive(Debug, Clone, Copy)]
pub enum Optimization {
    Debug,
    Release,
}

/// Writes `output` into `directory` and compiles every `.c` file at `optimization` with
/// warnings as errors, panicking with the compiler's output on failure.
pub fn compile(output: &Output, directory: &Path, optimization: Optimization) {
    output.write_to(directory).expect("emitted files written");
    let tool = compiler();
    let clang_cl = tool.is_like_clang_cl();
    assert!(
        clang_cl || tool.is_like_clang(),
        "generated C needs clang or clang-cl, found {:?}",
        tool.path()
    );
    let sources: Vec<PathBuf> = output
        .sources()
        .map(|file| directory.join(&file.name))
        .collect();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get());
    std::thread::scope(|scope| {
        for _ in 0..threads.min(sources.len()) {
            scope.spawn(|| {
                while let Some(source) = sources.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let object = source.with_extension("o");
                    let mut command = tool.to_command();
                    command.args(flags(clang_cl, optimization, &object));
                    command.arg(source);
                    if let Err(failure) = run(command, source) {
                        failures.lock().expect("no thread panics").push(failure);
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().expect("no thread panics");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Clang's `-Wall -Wextra` (clang-cl spells it `-W4`: its `-Wall` is `-Weverything`), C11,
/// warnings as errors, compile only.
fn flags(clang_cl: bool, optimization: Optimization, object: &Path) -> Vec<String> {
    let object = object.display();
    if clang_cl {
        let level = match optimization {
            Optimization::Debug => "-Od",
            Optimization::Release => "-O2",
        };
        vec![
            "-nologo".into(),
            "-c".into(),
            "-std:c11".into(),
            "-W4".into(),
            "-WX".into(),
            level.into(),
            format!("-Fo{object}"),
        ]
    } else {
        let level = match optimization {
            Optimization::Debug => "-O0",
            Optimization::Release => "-O2",
        };
        vec![
            "-c".into(),
            "-std=c11".into(),
            "-Wall".into(),
            "-Wextra".into(),
            "-Werror".into(),
            level.into(),
            "-o".into(),
            object.to_string(),
        ]
    }
}

/// Runs one compile; `Err` carries the compiler's output when it failed or warned.
fn run(mut command: Command, source: &Path) -> Result<(), String> {
    let result = command.output().expect("compiler starts");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() {
        Err(format!(
            "{} failed to compile:\n{stdout}{stderr}",
            source.display()
        ))
    } else if stderr.contains("warning") {
        Err(format!(
            "{} compiled with warnings:\n{stderr}",
            source.display()
        ))
    } else {
        Ok(())
    }
}

/// Finds clang-cl (Windows, from the Visual Studio LLVM component) or clang (Linux) the way
/// the game build will, through `cc`. Its warning flags stay on: `warnings(false)` makes cc
/// pass `-W0` to clang-cl, which silences every later `-W` flag.
fn compiler() -> cc::Tool {
    let target = target();
    let mut build = cc::Build::new();
    build
        .target(&target)
        .host(&target)
        .opt_level(0)
        .debug(false)
        .cargo_metadata(false);
    if target.ends_with("msvc") {
        build.prefer_clang_cl_over_msvc(true);
    } else {
        build.compiler("clang");
    }
    build.try_get_compiler().expect("a C compiler is found")
}

/// The target triple of this test binary; only the two supported hosts are known.
fn target() -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(all(windows, target_env = "msvc")) {
        format!("{arch}-pc-windows-msvc")
    } else if cfg!(all(target_os = "linux", target_env = "gnu")) {
        format!("{arch}-unknown-linux-gnu")
    } else {
        panic!("compile checks support Windows (MSVC) and Linux (GNU) hosts only")
    }
}

/// A scratch directory for one test's files.
pub fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("codegen-c")
        .join(name)
}
