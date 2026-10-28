//! The `recomp` command line.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anygenesis_analysis::{Analysis, analyze};
use anygenesis_codegen_c::LineMap;
use anygenesis_recomp::{build, disasm};
use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

const USAGE: &str = "usage:
  recomp disasm --rom <path> [--out <path>]
  recomp emit --rom <path> --out <directory> [--lines]";

/// The arguments both commands take.
#[derive(Debug)]
struct Args {
    rom: PathBuf,
    out: Option<PathBuf>,
    /// `emit` only: also write the listing and point `#line` directives at it.
    lines: bool,
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_ref().and_then(|command| command.to_str()) {
        Some("disasm") => {
            let args = parse_args(args)?;
            if args.lines {
                bail!("--lines is an emit option\n{USAGE}");
            }
            disasm_command(&args)
        }
        Some("emit") => emit_command(&parse_args(args)?),
        _ => bail!(USAGE),
    }
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<Args> {
    let (mut rom, mut out, mut lines) = (None, None, false);
    while let Some(flag) = args.next() {
        if flag == "--lines" {
            lines = true;
            continue;
        }
        let value = args
            .next()
            .with_context(|| format!("{flag:?} needs a value\n{USAGE}"))?;
        match flag.to_str() {
            Some("--rom") => rom = Some(PathBuf::from(value)),
            Some("--out") => out = Some(PathBuf::from(value)),
            _ => bail!("unknown argument {flag:?}\n{USAGE}"),
        }
    }
    Ok(Args {
        rom: rom.with_context(|| format!("--rom is required\n{USAGE}"))?,
        out,
        lines,
    })
}

/// Loads the ROM, printing its warnings.
fn load(args: &Args) -> Result<Rom> {
    let rom = build::load_rom(&args.rom)?;
    for warning in rom.warnings() {
        eprintln!("warning: {warning}");
    }
    Ok(rom)
}

fn disasm_command(args: &Args) -> Result<()> {
    let rom = load(args)?;
    let analysis = analyze(&rom);
    match &args.out {
        Some(path) => {
            let file =
                File::create(path).with_context(|| format!("creating {}", path.display()))?;
            let mut out = BufWriter::new(file);
            disasm::write_listing(&mut out, rom.bytes(), &analysis)?;
            out.flush()?;
        }
        None => {
            let mut out = BufWriter::new(io::stdout().lock());
            disasm::write_listing(&mut out, rom.bytes(), &analysis)?;
            out.flush()?;
        }
    }
    Ok(())
}

/// Writes the C a game build compiles into `--out`, for inspection.
fn emit_command(args: &Args) -> Result<()> {
    let directory = args
        .out
        .as_deref()
        .with_context(|| format!("--out is required\n{USAGE}"))?;
    let rom = load(args)?;
    let analysis = analyze(&rom);
    let mut options = build::game_options();
    if args.lines {
        options.lines = Some(write_listing_file(directory, args, &rom, &analysis)?);
    }
    let output = build::emit_program(&analysis, &rom, build::game_liveness(), &options);
    build::write_output(&output, directory)?;
    let lines: usize = output
        .files
        .iter()
        .map(|file| file.text.lines().count())
        .sum();
    println!(
        "{} functions, {} files, {lines} lines in {}",
        analysis.functions.len(),
        output.files.len(),
        directory.display()
    );
    Ok(())
}

/// Writes `<ROM file stem>.asm` into `directory` and returns where each instruction landed
/// in it.
fn write_listing_file(
    directory: &Path,
    args: &Args,
    rom: &Rom,
    analysis: &Analysis,
) -> Result<LineMap> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    let stem = args
        .rom
        .file_stem()
        .and_then(|stem| stem.to_str())
        .with_context(|| format!("{} has no UTF-8 file name", args.rom.display()))?;
    let file = format!("{stem}.asm");
    let path = directory.join(&file);
    let mut out = BufWriter::new(
        File::create(&path).with_context(|| format!("creating {}", path.display()))?,
    );
    let lines = disasm::write_listing(&mut out, rom.bytes(), analysis)?;
    out.flush()?;
    Ok(LineMap { file, lines })
}
