//! Fetches pinned SingleStepTests suites into the gitignored testdata/ directory.

use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};

use crate::workspace_root;

/// One SingleStepTests repository, pinned to a commit.
struct Suite {
    /// The name `cargo xtask fetch-sst <name>` takes.
    name: &'static str,
    repository: &'static str,
    /// Move it on purpose and re-run the suite's reports.
    commit: &'static str,
    destination: &'static str,
}

/// The first suite is the one fetched when no name is given.
const SUITES: &[Suite] = &[
    Suite {
        name: "m68000",
        repository: "https://github.com/SingleStepTests/m68000.git",
        // Upstream `main` as of 2024-08-01.
        commit: "64b253116a3de04aaac4346c43680960dc9b67e5",
        destination: "testdata/m68000",
    },
    Suite {
        name: "z80",
        repository: "https://github.com/SingleStepTests/z80.git",
        // Upstream `main` as of 2026-04-10.
        commit: "ebe1875d48f374bcfd4b505d8eb8ee751568b5f7",
        destination: "testdata/z80",
    },
];

/// Checks out the named suite's pinned commit (m68000 when `name` is `None`) into testdata/
/// with a shallow fetch, and is a no-op when it is already there.
///
/// # Errors
/// Returns an error if the name is unknown, if git fails, or if the destination exists but
/// is not a git checkout.
pub(crate) fn run(name: Option<&str>) -> Result<ExitCode> {
    let suite = match name {
        None => &SUITES[0],
        Some(name) => match SUITES.iter().find(|suite| suite.name == name) {
            Some(suite) => suite,
            None => bail!("unknown suite `{name}`; known: m68000, z80"),
        },
    };
    let Suite {
        name,
        repository,
        commit,
        destination: relative,
    } = suite;
    let destination = workspace_root()?.join(relative);
    if destination.join(".git").is_dir() {
        // An interrupted earlier fetch leaves a checkout without HEAD; fetch again.
        if head(&destination).is_ok_and(|head| head == *commit) {
            println!("{relative} is already at {commit}");
            return Ok(ExitCode::SUCCESS);
        }
    } else if destination.exists() {
        bail!("{relative} exists but is not a git checkout; remove it and retry");
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
        ["fetch", "--depth", "1", repository, commit],
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
    if fetched != *commit {
        bail!("{relative} is at {fetched} after the fetch, expected {commit}");
    }
    println!("fetched SingleStepTests/{name} {commit} into {relative}");
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
