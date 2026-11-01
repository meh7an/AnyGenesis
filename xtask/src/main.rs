//! Repository maintenance tasks, run as `cargo xtask <task>`.

mod fetch_sst;
mod legal;
mod notices;
mod patch_index;
mod release;
mod toolchain;

use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};

const USAGE: &str = "usage: cargo xtask <task>\n\ntasks:\n  legal                   reject ROM \
                     images and oversized files in the repository\n  fetch-sst [m68000|z80]  \
                     fetch a pinned SingleStepTests suite (default m68000) into testdata/\n  \
                     toolchain <zig archive>  build anygenesis and anygenesis-gui with \
                     the Zig toolchain and player library into target/bundle/\n  release \
                     [--dirty]       pack that bundle with notices into an installer in \
                     target/dist/\n  patch-index <clone>     write index.toml for the \
                     Marketplace into an AnyGenesis-Patches clone";

fn main() -> Result<ExitCode> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("legal") => legal::run(),
        Some("fetch-sst") => fetch_sst::run(args.get(1).map(String::as_str)),
        Some("toolchain") => toolchain::run(args.get(1).map(String::as_str)),
        Some("release") => release::run(args.get(1).map(String::as_str)),
        Some("patch-index") => patch_index::run(args.get(1).map(String::as_str)),
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
