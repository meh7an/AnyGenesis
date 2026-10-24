//! Repository maintenance tasks, run as `cargo xtask <task>`.

mod legal;

use std::{env, process::ExitCode};

use anyhow::{Result, bail};

const USAGE: &str = "usage: cargo xtask <task>\n\ntasks:\n  legal  reject ROM images and \
                     oversized files in the repository";

fn main() -> Result<ExitCode> {
    let task = env::args().nth(1);
    match task.as_deref() {
        Some("legal") => legal::run(),
        Some(other) => bail!("unknown task `{other}`\n\n{USAGE}"),
        None => bail!(USAGE),
    }
}
