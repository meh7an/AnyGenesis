//! The recompile pipeline build scripts and the CLI share: a ROM in, translated C out, and
//! that C compiled with clang into a static library. Nothing but the ROM decides what is
//! built: any image the loader accepts translates.
//!
//! Generated code needs clang (clang-cl on MSVC targets): cross-function jumps rely on
//! `musttail` for guaranteed tail calls.

use std::{
    env, io,
    path::{Path, PathBuf},
};

use anygenesis_analysis::{Analysis, FlagLiveness, LivenessOptions, Origin, analyze};
use anygenesis_codegen_c::{CheckPoints, EmitOptions, Output, emit};
use anygenesis_rom::{Rom, RomError};

/// Why the pipeline stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("loading the ROM {}: {source}", path.display())]
    Rom { path: PathBuf, source: RomError },
    #[error("writing {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("translated C needs clang or clang-cl, found {}", path.display())]
    NotClang { path: PathBuf },
    #[error("compiling {library}: {source}")]
    Compile { library: String, source: cc::Error },
}

/// An analysed game and its translation.
#[derive(Debug)]
pub struct Translation {
    pub analysis: Analysis,
    pub output: Output,
}

/// Loads the ROM image at `path`.
///
/// # Errors
/// Returns [`BuildError::Rom`] for a missing or malformed image.
pub fn load_rom(path: &Path) -> Result<Rom, BuildError> {
    Rom::load(path).map_err(|source| BuildError::Rom {
        path: path.to_owned(),
        source,
    })
}

/// The options a game executable is built with: cycle accounting, a check point at the start
/// of every block, and stepped code for every kept ROM address, which the check points'
/// slow paths run (native everywhere). Interrupts then arrive at the instruction boundary
/// the interpreter takes them at, and no address in ROM lacks native code.
pub fn game_options() -> EmitOptions {
    EmitOptions {
        cycles: true,
        check_points: CheckPoints::BlockStarts,
        stepped: true,
        ..EmitOptions::default()
    }
}

/// The flag liveness a game executable is built with: every check point reads the whole
/// status register, so the frame an interrupt pushes there holds exact flags.
pub fn game_liveness() -> LivenessOptions {
    LivenessOptions {
        check_points_read_sr: true,
    }
}

/// Emits C for every function of `analysis`, with the flags `liveness` proves dead pruned;
/// `rom` is the image `analysis` came from.
pub fn emit_program(
    analysis: &Analysis,
    rom: &Rom,
    liveness: LivenessOptions,
    options: &EmitOptions,
) -> Output {
    let liveness = FlagLiveness::new(analysis, liveness);
    emit(analysis, &liveness, options, rom.bytes())
}

/// Analyses `rom` from its vectors and translates it.
pub fn translate(rom: &Rom, liveness: LivenessOptions, options: &EmitOptions) -> Translation {
    let analysis = analyze(rom);
    let output = emit_program(&analysis, rom, liveness, options);
    Translation { analysis, output }
}

/// Returns the entries of the functions reset and the exception vectors start, ascending.
pub fn vector_handlers(analysis: &Analysis) -> Vec<u32> {
    analysis
        .functions
        .values()
        .filter(|function| {
            function
                .origins
                .iter()
                .any(|origin| matches!(origin, Origin::Reset | Origin::Vector(_)))
        })
        .map(|function| function.entry.get())
        .collect()
}

/// Writes `output` into `directory` and returns the paths of its `.c` files.
///
/// # Errors
/// Returns [`BuildError::Write`].
pub fn write_output(output: &Output, directory: &Path) -> Result<Vec<PathBuf>, BuildError> {
    output
        .write_to(directory)
        .map_err(|source| BuildError::Write {
            path: directory.to_owned(),
            source,
        })?;
    Ok(output
        .sources()
        .map(|file| directory.join(&file.name))
        .collect())
}

/// Compiles `sources` as C11 with clang-cl on MSVC targets and clang elsewhere, in parallel,
/// with every warning an error, and links the static library `library` into the crate whose
/// build script calls it. Headers are looked up in `include`.
///
/// # Errors
/// Returns [`BuildError::NotClang`] when cc finds another compiler, and
/// [`BuildError::Compile`] when compiling or archiving fails.
pub fn compile(library: &str, include: &Path, sources: &[PathBuf]) -> Result<(), BuildError> {
    let target = env::var("TARGET").unwrap_or_default();
    clang_build(cc::Build::new(), &target, library)?
        .include(include)
        .files(sources)
        .try_compile(library)
        .map_err(|source| compile_error(library, source))
}

/// Compiles a game's `sources` with headers from `include` into the static library `library`,
/// as [`compile`] does, but the stepped files (`ag_sNNN.c`), most of the C and run only on
/// slow paths, at the optimisation level `stepped` (cc's names: "0"-"3", "s", "z") when it
/// is given; the rest at the profile's level.
///
/// # Errors
/// As [`compile`].
pub fn compile_game(
    library: &str,
    include: &Path,
    sources: &[PathBuf],
    stepped: Option<&str>,
) -> Result<(), BuildError> {
    let Some(level) = stepped else {
        return compile(library, include, sources);
    };
    let target = env::var("TARGET").unwrap_or_default();
    let (steps, rest): (Vec<&PathBuf>, Vec<&PathBuf>) =
        sources.iter().partition(|path| is_stepped_file(path));
    let objects = clang_build(cc::Build::new(), &target, library)?
        .include(include)
        .files(steps)
        .opt_level_str(level)
        .try_compile_intermediates()
        .map_err(|source| compile_error(library, source))?;
    clang_build(cc::Build::new(), &target, library)?
        .include(include)
        .files(rest)
        .objects(objects)
        .try_compile(library)
        .map_err(|source| compile_error(library, source))
}

/// Whether `path` is a stepped file the emitter wrote (`ag_s000.c`, ...; not `ag_steps.c`).
fn is_stepped_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("ag_s"))
        .and_then(|rest| rest.strip_suffix(".c"))
        .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Configures `build` for translated C on `target`: clang, C11, warnings as errors.
fn clang_build(mut build: cc::Build, target: &str, library: &str) -> Result<cc::Build, BuildError> {
    if target.ends_with("msvc") {
        build.prefer_clang_cl_over_msvc(true);
    } else {
        build.compiler("clang");
    }
    let tool = build
        .try_get_compiler()
        .map_err(|source| compile_error(library, source))?;
    if !tool.is_like_clang_cl() && !tool.is_like_clang() {
        return Err(BuildError::NotClang {
            path: tool.path().to_owned(),
        });
    }
    // cc's default warning level is on (-W4 / -Wall -Wextra); `warnings(false)` would pass
    // -W0 to clang-cl and silence it.
    build.std("c11").warnings_into_errors(true);
    Ok(build)
}

fn compile_error(library: &str, source: cc::Error) -> BuildError {
    BuildError::Compile {
        library: library.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const TARGET: &str = if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };

    /// Compiles `text` as a library of its own outside any build script.
    fn compile_text(name: &str, text: &str) -> Result<(), BuildError> {
        let directory = env::temp_dir().join(format!("anygenesis-recomp-{name}"));
        fs::create_dir_all(&directory).expect("temp directory created");
        let source = directory.join(format!("{name}.c"));
        fs::write(&source, text).expect("source written");
        let mut build = cc::Build::new();
        build
            .target(TARGET)
            .host(TARGET)
            .opt_level(0)
            .debug(false)
            .out_dir(&directory)
            .cargo_metadata(false);
        clang_build(build, TARGET, name)?
            .file(&source)
            .try_compile(name)
            .map_err(|source| compile_error(name, source))
    }

    #[test]
    fn test_only_numbered_stepped_files_count_as_stepped() {
        let stepped = |name: &str| is_stepped_file(Path::new("c").join(name).as_path());
        assert!(stepped("ag_s000.c"));
        assert!(stepped("ag_s219.c"));
        assert!(!stepped("ag_steps.c"));
        assert!(!stepped("ag_000.c"));
        assert!(!stepped("ag_table.c"));
        assert!(!stepped("ag_s000.h"));
    }

    #[test]
    fn test_compile_clean_source_succeeds() {
        let result = compile_text("clean", "int ag_clean(int x) { return x + 1; }\n");
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn test_compile_warning_fails() {
        let text = "int ag_warns(int x) { int unused; return x; }\n";
        let result = compile_text("warns", text);
        assert!(
            matches!(result, Err(BuildError::Compile { .. })),
            "{result:?}"
        );
    }
}
