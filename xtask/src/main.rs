//! Repository maintenance tasks, run as `cargo xtask <task>`.

mod fetch_sst;
mod legal;

use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};

const USAGE: &str = "usage: cargo xtask <task>\n\ntasks:\n  legal      reject ROM images and \
                     oversized files in the repository\n  fetch-sst  fetch the pinned \
                     SingleStepTests/m68000 suite into testdata/";

fn main() -> Result<ExitCode> {
    let task = env::args().nth(1);
    match task.as_deref() {
        Some("legal") => legal::run(),
        Some("fetch-sst") => fetch_sst::run(),
        Some(other) => bail!("unknown task `{other}`\n\n{USAGE}"),
        None => bail!(USAGE),
    }
}

fn workspace_root() -> Result<PathBuf> {
    // xtask always sits one directory below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest directory has no parent")
}
