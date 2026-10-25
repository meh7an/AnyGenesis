//! The recompiled game: translated 68000 code linked against the runtime.
//!
//! The binary loads and checks the ROM, checks the translated program's address table and
//! runs from reset for a number of frames on the stub VDP, which raises the vertical and
//! horizontal interrupts but draws nothing. It reports where the run ended and what the
//! runtime logged on the way.

mod game;

use std::{
    env,
    path::{Path, PathBuf},
    thread,
};

use anygenesis_abi::{AgEntry, ram_index};
use anygenesis_rom::Rom;
use anygenesis_runtime::{Access, Exit, Miss, Options, RUN_STACK, Runtime, StubVdp};
use anyhow::{Context, Result, anyhow, bail};

use crate::game::CompiledGame;

const ROM_VAR: &str = "ANYGENESIS_ROM";
const USAGE: &str = "usage: anygenesis [<rom.bin> [<frames>]] (the ROM may come from \
                     ANYGENESIS_ROM)";
/// Ten seconds of NTSC frames.
const DEFAULT_FRAMES: u64 = 600;
/// The work RAM byte the report shows; Sonic 1 keeps its game mode there.
const WATCHED_BYTE: u32 = 0xFF_F600;

fn main() -> Result<()> {
    let Some(game) = game::compiled() else {
        bail!("built without a game: set GAME and {ROM_VAR}, then rebuild");
    };
    let path = rom_path()?;
    let frames = frame_count()?;
    let rom = load_rom(&game, &path)?;
    println!("game {}, ROM SHA-1 {}", game.name, game.sha1);
    println!(
        "{} functions, {} table rows, {} vector handlers",
        game.functions,
        game.table.len(),
        game.vector_handlers.len()
    );
    let report = run_from_reset(game.table, rom, frames)?;
    report.print();
    Ok(())
}

/// The ROM image: the first argument, else `ANYGENESIS_ROM`.
fn rom_path() -> Result<PathBuf> {
    env::args_os()
        .nth(1)
        .or_else(|| env::var_os(ROM_VAR))
        .map(PathBuf::from)
        .context(USAGE)
}

/// Frames to run: the second argument, else ten seconds' worth.
fn frame_count() -> Result<u64> {
    let Some(argument) = env::args().nth(2) else {
        return Ok(DEFAULT_FRAMES);
    };
    argument
        .parse()
        .with_context(|| format!("{argument:?} is not a frame count\n{USAGE}"))
}

/// Loads the ROM at `path`, refusing any but the one `game` was translated from, and checks
/// that the address table covers its reset PC.
fn load_rom(game: &CompiledGame, path: &Path) -> Result<Rom> {
    let rom = Rom::load(path).with_context(|| format!("loading {}", path.display()))?;
    let sha1 = rom.sha1().to_string();
    if sha1 != game.sha1 {
        bail!(
            "{} has SHA-1 {sha1}, but this build of {} was translated from {}",
            path.display(),
            game.name,
            game.sha1
        );
    }
    game.check_table(rom.vectors().reset_pc() & 0xFF_FFFF)?;
    Ok(rom)
}

/// What a run from reset ended with, and what the runtime saw on the way.
#[derive(Debug)]
struct Report {
    ssp: u32,
    reset_pc: u32,
    exit: Exit,
    cycles: u64,
    frames: u64,
    vblanks: u64,
    hblanks: u64,
    /// The status register's system byte at the end.
    sr: u16,
    watched: u8,
    misses: Vec<(u32, Miss)>,
    accesses: Vec<(Access, u64)>,
    unwinds: u64,
    tmss_unlocked: bool,
    /// Z80 RAM bytes that are not zero.
    z80_bytes: usize,
    vdp: StubVdp,
}

impl Report {
    fn print(&self) {
        println!("reset: SSP ${:08X}, PC ${:06X}", self.ssp, self.reset_pc);
        println!(
            "run ended: {} after {} cycles, {} frames",
            self.exit, self.cycles, self.frames
        );
        println!(
            "interrupts taken: {} VBlank (level 6), {} HBlank (level 4); SR ${:04X}, RAM \
             ${WATCHED_BYTE:06X} = ${:02X}",
            self.vblanks, self.hblanks, self.sr, self.watched
        );
        println!(
            "TMSS {}, Z80 RAM {} bytes set, native stack unwinds {}",
            if self.tmss_unlocked {
                "unlocked"
            } else {
                "locked"
            },
            self.z80_bytes,
            self.unwinds
        );
        let vdp = &self.vdp;
        println!(
            "stub VDP: {} register writes, {} command words, {} data writes, {} status reads, \
             {} PSG writes",
            vdp.register_writes,
            vdp.command_writes,
            vdp.data_writes,
            vdp.status_reads,
            vdp.psg_writes
        );
        println!("misses (hint candidates): {}", self.misses.len());
        for (address, miss) in &self.misses {
            println!(
                "[[function]]\naddress = 0x{address:06X}  # {} lookups, {} resumes, {} \
                 instructions interpreted",
                miss.lookups, miss.resumes, miss.instructions
            );
        }
        println!("logged accesses: {}", self.accesses.len());
        for (access, count) in &self.accesses {
            println!("  {access} x{count}");
        }
    }
}

/// Runs from reset on a thread with room for the native stack until `frames` frames have
/// ended, or the run stops earlier at a wait nothing can end.
fn run_from_reset(table: &'static [AgEntry], rom: Rom, frames: u64) -> Result<Report> {
    let worker = thread::Builder::new()
        .name("68000".into())
        .stack_size(RUN_STACK)
        .spawn(move || {
            let options = Options {
                stop_when_idle: true,
                ..Options::default()
            };
            let mut runtime = Runtime::new(rom, table, Box::<StubVdp>::default(), options);
            let ssp = runtime.state().a[7];
            let reset_pc = runtime.state().pc;
            let exit = runtime.run_frames(frames);
            let state = runtime.state();
            Report {
                ssp,
                reset_pc,
                exit,
                cycles: runtime.cycles(),
                frames: runtime.frames(),
                vblanks: runtime.interrupts(6),
                hblanks: runtime.interrupts(4),
                sr: state.sr,
                watched: state.ram[ram_index(WATCHED_BYTE)],
                misses: runtime
                    .log()
                    .misses()
                    .iter()
                    .map(|(&address, &miss)| (address, miss))
                    .collect(),
                accesses: runtime.log().accesses(),
                unwinds: runtime.log().unwinds(),
                tmss_unlocked: runtime.tmss_unlocked(),
                z80_bytes: runtime.z80_ram().iter().filter(|&&byte| byte != 0).count(),
                vdp: runtime.hardware::<StubVdp>().cloned().unwrap_or_default(),
            }
        })
        .context("starting the 68000 thread")?;
    worker
        .join()
        .map_err(|_| anyhow!("the 68000 thread panicked"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sonic 1 from reset for ten seconds of frames on the stub VDP, with interrupts.
    #[test]
    #[ignore = "needs a GAME=sonic1 build and ANYGENESIS_ROM"]
    fn test_sonic1_runs_600_frames_without_misses() {
        let game = game::compiled().expect("the game crate was built with GAME set");
        let path = env::var_os(ROM_VAR).expect("ANYGENESIS_ROM is set");
        let rom = load_rom(&game, Path::new(&path)).expect("the ROM matches the build");
        let report = run_from_reset(game.table, rom, DEFAULT_FRAMES).expect("the run finishes");
        report.print();
        assert!(
            matches!(report.exit, Exit::Frames { .. }),
            "{}",
            report.exit
        );
        assert_eq!(report.frames, DEFAULT_FRAMES);
        assert!(report.misses.is_empty(), "{:?}", report.misses);
        assert!(report.accesses.is_empty(), "{:?}", report.accesses);
        assert_eq!(report.unwinds, 0);
        assert!(report.vblanks > 0);
        assert!(report.tmss_unlocked);
        assert!(report.vdp.register_writes > 0);
        assert!(report.z80_bytes > 0);
    }
}
