//! `framediff`: writes the reference emulator's debugger script, and diffs two runs' dumps.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anygenesis_framediff::{Folder, Input, Masks, diff, script};
use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

const USAGE: &str = "usage:\n  \
    framediff script <rom.bin> <stops> [--input <file>]   prints the reference's debugger \
    script\n  \
    framediff diff <ours> <reference> [--masks <file>]    reports the first divergence";

/// The level of the vertical interrupt, whose handler entry is every stop.
const VBLANK_LEVEL: u8 = 6;

fn main() -> Result<ExitCode> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next().context(USAGE)?;
    let flag = match command.to_str() {
        Some("script") => "--input",
        Some("diff") => "--masks",
        _ => bail!(USAGE),
    };
    let (positional, option) = split(arguments, flag)?;
    match (command.to_str(), positional.as_slice()) {
        (Some("script"), [rom, stops]) => {
            print!(
                "{}",
                reference_script(Path::new(rom), stops, option.as_deref())?
            );
            Ok(ExitCode::SUCCESS)
        }
        (Some("diff"), [ours, reference]) => {
            let masks = match option {
                Some(path) => Masks::load(&path)?,
                None => Masks::default(),
            };
            let mut ours = Folder(PathBuf::from(ours));
            let mut reference = Folder(PathBuf::from(reference));
            let report = diff(&mut ours, &mut reference, &masks)?;
            print!("{report}");
            if !masks.masks().is_empty() {
                println!("masks in force: {}", masks.masks().len());
            }
            Ok(if report.divergence.is_some() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        _ => bail!(USAGE),
    }
}

/// Splits the arguments into positional ones and the value of `flag`, the one option the
/// command takes.
fn split(
    mut arguments: impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<(Vec<OsString>, Option<PathBuf>)> {
    let mut positional = Vec::new();
    let mut option = None;
    while let Some(argument) = arguments.next() {
        if argument == flag {
            let value = arguments.next().context(USAGE)?;
            if option.replace(PathBuf::from(value)).is_some() {
                bail!(USAGE);
            }
        } else {
            positional.push(argument);
        }
    }
    Ok((positional, option))
}

/// The debugger script for `stops` stops of the reference run of `rom`.
fn reference_script(rom: &Path, stops: &OsString, input: Option<&Path>) -> Result<String> {
    let stops: u64 = stops
        .to_str()
        .and_then(|stops| stops.parse().ok())
        .with_context(|| format!("{stops:?} is not a stop count\n{USAGE}"))?;
    let rom = Rom::load(rom).with_context(|| format!("loading {}", rom.display()))?;
    let handler = rom
        .vectors()
        .autovector(VBLANK_LEVEL)
        .context("the ROM has no vertical interrupt vector")?;
    let input = match input {
        Some(path) => Input::load(path)?,
        None => Input::default(),
    };
    Ok(script::blastem(handler & 0xFF_FFFF, stops, &input))
}
