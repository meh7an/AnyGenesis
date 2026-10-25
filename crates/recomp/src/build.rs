//! The recompile pipeline build scripts and the CLI share: a game's config and ROM in,
//! translated C out, and that C compiled with clang into a static library.
//!
//! Generated code needs clang (clang-cl on MSVC targets): cross-function jumps rely on
//! `musttail` for guaranteed tail calls.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anygenesis_analysis::{
    Analysis, AnalysisError, FlagLiveness, LivenessOptions, Origin, analyze,
};
use anygenesis_codegen_c::{CheckPoints, EmitOptions, Output, emit};
use anygenesis_gamecfg::{ConfigError, GameConfig};
use anygenesis_rom::{Rom, RomError};

/// Game configs ship in the workspace's games/ folder; the path is fixed at build time.
pub const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../games");

/// Why the pipeline stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("game name {name:?} must be letters, digits and underscores")]
    BadGameName { name: String },
    #[error("unknown game {name:?}: games/ holds {}", list(available))]
    UnknownGame {
        name: String,
        available: Vec<String>,
    },
    #[error("reading {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("in {}:\n{source}", path.display())]
    Config { path: PathBuf, source: ConfigError },
    #[error("loading the ROM {}: {source}", path.display())]
    Rom { path: PathBuf, source: RomError },
    #[error(transparent)]
    Analysis(#[from] AnalysisError),
    #[error("writing {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("translated C needs clang or clang-cl, found {}", path.display())]
    NotClang { path: PathBuf },
    #[error("compiling {library}: {source}")]
    Compile { library: String, source: cc::Error },
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "no configs".to_owned()
    } else {
        names.join(", ")
    }
}

/// An analysed game and its translation.
#[derive(Debug)]
pub struct Translation {
    pub analysis: Analysis,
    pub output: Output,
}

/// Returns the path of the config for the game `name` in [`GAMES_DIR`].
///
/// # Errors
/// Returns [`BuildError::BadGameName`] for a name that could leave games/, and
/// [`BuildError::UnknownGame`] when no config has that name.
pub fn config_path(name: &str) -> Result<PathBuf, BuildError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(BuildError::BadGameName {
            name: name.to_owned(),
        });
    }
    let path = Path::new(GAMES_DIR).join(format!("{name}.toml"));
    if path.is_file() {
        Ok(path)
    } else {
        Err(BuildError::UnknownGame {
            name: name.to_owned(),
            available: available_games(),
        })
    }
}

/// The game names games/ holds, sorted.
fn available_games() -> Vec<String> {
    let Ok(entries) = fs::read_dir(GAMES_DIR) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "toml" {
                return None;
            }
            Some(path.file_stem()?.to_str()?.to_owned())
        })
        .collect();
    names.sort();
    names
}

/// Reads and validates the config at `path`.
///
/// # Errors
/// Returns [`BuildError::Read`] or [`BuildError::Config`].
pub fn load_config(path: &Path) -> Result<GameConfig, BuildError> {
    let text = fs::read_to_string(path).map_err(|source| BuildError::Read {
        path: path.to_owned(),
        source,
    })?;
    GameConfig::parse(&text).map_err(|source| BuildError::Config {
        path: path.to_owned(),
        source,
    })
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

/// The options a game executable is built with: cycle accounting and a check point at every
/// loop target, the hooks the runtime's scheduler needs.
pub fn game_options() -> EmitOptions {
    EmitOptions {
        cycles: true,
        check_points: CheckPoints::LoopTargets,
        ..EmitOptions::default()
    }
}

/// Emits C for every function of `analysis`, with dead flags pruned.
pub fn emit_program(analysis: &Analysis, options: &EmitOptions) -> Output {
    let liveness = FlagLiveness::new(analysis, LivenessOptions::default());
    emit(analysis, &liveness, options)
}

/// Analyses `rom` as `config` describes it and translates it.
///
/// # Errors
/// Returns [`BuildError::Analysis`] when `config` belongs to another ROM image.
pub fn translate(
    rom: &Rom,
    config: &GameConfig,
    options: &EmitOptions,
) -> Result<Translation, BuildError> {
    let analysis = analyze(rom, config)?;
    let output = emit_program(&analysis, options);
    Ok(Translation { analysis, output })
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
