//! The `recomp` command line.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anygenesis_analysis::analyze;
use anygenesis_gamecfg::GameConfig;
use anygenesis_recomp::disasm;
use anygenesis_rom::Rom;
use anyhow::{Context, Result, bail};

const USAGE: &str = "usage: recomp disasm --rom <path> --game <name> [--out <path>]";

/// Game configs ship in the workspace's games/ folder; the path is fixed at build time.
const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../games");

/// The arguments of `recomp disasm`.
#[derive(Debug)]
struct DisasmArgs {
    rom: PathBuf,
    game: String,
    out: Option<PathBuf>,
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_ref().and_then(|command| command.to_str()) {
        Some("disasm") => disasm_command(parse_disasm(args)?),
        _ => bail!(USAGE),
    }
}

fn parse_disasm(mut args: impl Iterator<Item = OsString>) -> Result<DisasmArgs> {
    let (mut rom, mut game, mut out) = (None, None, None);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag:?} needs a value\n{USAGE}"))?;
        match flag.to_str() {
            Some("--rom") => rom = Some(PathBuf::from(value)),
            Some("--game") => {
                game = Some(
                    value
                        .into_string()
                        .map_err(|value| anyhow::anyhow!("game name {value:?} is not UTF-8"))?,
                );
            }
            Some("--out") => out = Some(PathBuf::from(value)),
            _ => bail!("unknown argument {flag:?}\n{USAGE}"),
        }
    }
    Ok(DisasmArgs {
        rom: rom.with_context(|| format!("--rom is required\n{USAGE}"))?,
        game: game.with_context(|| format!("--game is required\n{USAGE}"))?,
        out,
    })
}

fn disasm_command(args: DisasmArgs) -> Result<()> {
    let rom = Rom::load(&args.rom).with_context(|| format!("loading {}", args.rom.display()))?;
    for warning in rom.warnings() {
        eprintln!("warning: {warning}");
    }
    let config = load_config(&args.game)?;
    let analysis = analyze(&rom, &config)?;
    match &args.out {
        Some(path) => {
            let file =
                File::create(path).with_context(|| format!("creating {}", path.display()))?;
            let mut out = BufWriter::new(file);
            disasm::write_listing(&mut out, rom.bytes(), &config, &analysis)?;
            out.flush()?;
        }
        None => {
            let mut out = BufWriter::new(io::stdout().lock());
            disasm::write_listing(&mut out, rom.bytes(), &config, &analysis)?;
            out.flush()?;
        }
    }
    Ok(())
}

fn load_config(game: &str) -> Result<GameConfig> {
    // A plain name keeps the lookup inside games/.
    if game.is_empty()
        || !game
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("game name {game:?} must be letters, digits and underscores");
    }
    let path = Path::new(GAMES_DIR).join(format!("{game}.toml"));
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    GameConfig::parse(&text).with_context(|| format!("in {}", path.display()))
}
