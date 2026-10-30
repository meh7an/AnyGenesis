//! Profile runs: a game run on the interpreter until its code stops growing, for the build
//! to learn which code the game really runs.

use crate::{Exit, Runtime};

/// Frames run between looks at the profile.
const CHUNK: u64 = 60;

/// When a profile run ends, in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiet {
    /// The run lasts at least this long.
    pub least: u64,
    /// It ends once this many frames have run no instruction address not run before.
    pub quiet: u64,
    /// It ends here at the latest.
    pub most: u64,
}

impl Quiet {
    /// The build's profile run: at least a minute, ended by a minute with no new code, ten
    /// minutes at most (at 60 frames per second).
    pub const BUILD: Self = Self {
        least: 3600,
        quiet: 3600,
        most: 36_000,
    };
}

/// Runs `runtime`, which must be keeping a profile ([`Runtime::record_profile`]), until
/// `quiet` ends the run or the run ends on its own, and returns how it ended.
///
/// # Panics
/// If `runtime` keeps no profile.
pub fn run_until_quiet(runtime: &mut Runtime, quiet: Quiet) -> Exit {
    let distinct = |runtime: &Runtime| {
        runtime
            .profile()
            .expect("the run keeps a profile")
            .distinct()
    };
    let start = runtime.frames();
    let (mut known, mut grew) = (distinct(runtime), start);
    loop {
        let frames = runtime.frames() - start;
        let left = quiet.most - frames;
        let exit = runtime.run_frames(CHUNK.min(left));
        let now = runtime.frames();
        let seen = distinct(runtime);
        if seen > known {
            (known, grew) = (seen, now);
        }
        let ran = now - start;
        let settled = ran >= quiet.least && now - grew >= quiet.quiet;
        if !matches!(exit, Exit::Frames { .. }) || settled || ran >= quiet.most {
            return exit;
        }
    }
}
