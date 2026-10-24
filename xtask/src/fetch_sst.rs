//! Fetches the SingleStepTests/m68000 vectors into the gitignored testdata/ directory.

use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};

use crate::workspace_root;

const REPOSITORY: &str = "https://github.com/SingleStepTests/m68000.git";

/// Upstream `main` as of 2024-08-01. Move it on purpose and re-run the Musashi report.
const COMMIT: &str = "64b253116a3de04aaac4346c43680960dc9b67e5";

const DESTINATION: &str = "testdata/m68000";

/// Checks out the pinned commit into testdata/m68000 with a shallow fetch, and is a no-op
/// when it is already there.
///
/// # Errors
/// Returns an error if git fails, or if the destination exists but is not a git checkout.
pub(crate) fn run() -> Result<ExitCode> {
    let destination = workspace_root()?.join(DESTINATION);
    if destination.join(".git").is_dir() {
        // An interrupted earlier fetch leaves a checkout without HEAD; fetch again.
        if head(&destination).is_ok_and(|head| head == COMMIT) {
            println!("{DESTINATION} is already at {COMMIT}");
            return Ok(ExitCode::SUCCESS);
        }
    } else if destination.exists() {
        bail!("{DESTINATION} exists but is not a git checkout; remove it and retry");
    } else {
        git(
            None,
            [
                OsStr::new("init"),
                OsStr::new("-q"),
                destination.as_os_str(),
            ],
        )?;
        // The vectors are binary; keep every file byte-identical to upstream.
        git(Some(&destination), ["config", "core.autocrlf", "false"])?;
    }

    git(
        Some(&destination),
        ["fetch", "--depth", "1", REPOSITORY, COMMIT],
    )?;
    git(
        Some(&destination),
        [
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "-q",
            "--force",
            "FETCH_HEAD",
        ],
    )?;

    let fetched = head(&destination)?;
    if fetched != COMMIT {
        bail!("{DESTINATION} is at {fetched} after the fetch, expected {COMMIT}");
    }
    println!("fetched SingleStepTests/m68000 {COMMIT} into {DESTINATION}");
    Ok(ExitCode::SUCCESS)
}

fn git<I, S>(directory: Option<&Path>, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    if let Some(directory) = directory {
        command.arg("-C").arg(directory);
    }
    let status = command.args(args).status().context("running git")?;
    if !status.success() {
        bail!("git failed with {status}");
    }
    Ok(())
}

fn head(checkout: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("running git rev-parse")?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
