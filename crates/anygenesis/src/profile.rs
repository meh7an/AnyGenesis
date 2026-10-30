//! How far the build discovers code, and the profile run behind the furthest stage: the game
//! run headless on the interpreter from reset with no input (title, attract mode, demos)
//! until its code stops growing. The computed jumps and calls it took become optimised code
//! in the build; the profile lives only in memory and in the cache entry.

use std::{fs, path::Path, thread, time::Instant};

use anygenesis_analysis::{Analysis, DiscoveryStage, analyze_profiled, analyze_stage};
use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Exit, Options, RUN_STACK, Runtime, Vdp,
    profile::{Quiet, run_until_quiet},
};
use anygenesis_shared::Profile;
use anyhow::{Context, Result, anyhow};

/// The file in a cache entry that keeps the profile the build followed.
const PROFILE_FILE: &str = "profile.txt";

/// How far a build discovers code before the superset covers the rest with stepped code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Discovery {
    /// The descent from the vectors alone.
    Vectors,
    /// The descent and the jump tables the recogniser finds.
    Static,
    /// As static, plus the code a profile run reached through computed jumps and calls.
    #[default]
    Profile,
}

impl Discovery {
    /// The stage `--discovery` names, if any.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "vectors" => Some(Self::Vectors),
            "static" => Some(Self::Static),
            "profile" => Some(Self::Profile),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Vectors => "vectors",
            Self::Static => "static",
            Self::Profile => "profile",
        }
    }
}

/// Discovers the code of `rom` as far as `discovery` goes; a profile run writes its profile
/// into `folder` and prints what it ran.
///
/// # Errors
/// The profile run's thread failing, or writing the profile.
pub fn analyze(rom: &Rom, discovery: Discovery, folder: &Path) -> Result<Analysis> {
    Ok(match discovery {
        Discovery::Vectors => analyze_stage(rom, DiscoveryStage::Vectors),
        Discovery::Static => analyze_stage(rom, DiscoveryStage::Static),
        Discovery::Profile => {
            let started = Instant::now();
            let (profile, frames, exit) = record(rom.clone(), Quiet::BUILD)?;
            println!(
                "profile run: {frames} frames on the interpreter in {:.1} s ({exit:?}), {} \
                 addresses run, {} computed jumps and calls",
                started.elapsed().as_secs_f64(),
                profile.distinct(),
                profile.computed_targets().len()
            );
            let path = folder.join(PROFILE_FILE);
            fs::write(&path, profile.text(frames))
                .with_context(|| format!("writing {}", path.display()))?;
            analyze_profiled(rom, &profile)
        }
    })
}

/// Runs `rom` from reset on the interpreter with no input until `quiet` ends the run, and
/// returns its profile, the frames it ran and how it ended.
///
/// # Errors
/// The run's thread failing to start or panicking.
pub fn record(rom: Rom, quiet: Quiet) -> Result<(Profile, u64, Exit)> {
    thread::Builder::new()
        .name("profile".into())
        .stack_size(RUN_STACK)
        .spawn(move || {
            let mut runtime = interpreted(rom);
            runtime.record_profile();
            let exit = run_until_quiet(&mut runtime, quiet);
            let frames = runtime.frames();
            let profile = runtime.take_profile().expect("the run keeps a profile");
            (profile, frames, exit)
        })
        .context("starting the profile run")?
        .join()
        .map_err(|_| anyhow!("the profile run panicked"))
}

/// The runtime for `rom` at reset with no native code, so the interpreter runs every
/// instruction; options as a game's own headless run.
pub fn interpreted(rom: Rom) -> Runtime {
    let options = Options {
        stop_when_idle: true,
        ..Options::default()
    };
    Runtime::new(rom, &[], Box::<Vdp>::default(), options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_names_parse_back_and_profile_is_the_default() {
        for discovery in [Discovery::Vectors, Discovery::Static, Discovery::Profile] {
            assert_eq!(Discovery::parse(discovery.name()), Some(discovery));
        }
        assert_eq!(Discovery::parse("fast"), None);
        assert_eq!(Discovery::default(), Discovery::Profile);
    }
}
