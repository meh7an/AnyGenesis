//! Machine-readable progress lines for `--progress`, read by anygenesis-gui: one line per
//! build phase, `ag-progress <phase>` or `ag-progress compile <done> <total>`, then
//! `ag-done <exe>` or `ag-error <message>`. The usual output lines stay between them.

use std::{
    fmt::Display,
    sync::atomic::{AtomicBool, Ordering},
};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// The build phases, in the order a build runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Read,
    Patch,
    Profile,
    Translate,
    Link,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Patch => "patch",
            Self::Profile => "profile",
            Self::Translate => "translate",
            Self::Link => "link",
        }
    }
}

/// Turns the progress lines on for the rest of the run.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Whether `--progress` was given.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Reports that `phase` starts.
pub fn phase(phase: Phase) {
    if enabled() {
        println!("ag-progress {}", phase.name());
    }
}

/// Reports that `done` of `total` C files are compiled.
pub fn compiled(done: usize, total: usize) {
    if enabled() {
        println!("ag-progress compile {done} {total}");
    }
}

/// Reports the finished game program.
pub fn done(exe: impl Display) {
    if enabled() {
        println!("ag-done {exe}");
    }
}

/// Reports the error that stopped the build, on one line.
pub fn error(error: &anyhow::Error) {
    if enabled() {
        let message = format!("{error:#}").replace(['\r', '\n'], " ");
        println!("ag-error {message}");
    }
}
