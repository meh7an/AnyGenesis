//! The recompiled game: translated 68000 code linked against the runtime.
//!
//! The binary loads and checks the ROM, checks the translated program's address table and
//! runs from reset for a number of frames on the VDP, which keeps its memories, runs DMA,
//! raises the vertical and horizontal interrupts and draws the picture. It reports where the
//! run ended, what the VDP holds and what the runtime logged on the way, and saves the
//! frames asked for with `--dump` as BMP files in `dumps/` under the working directory.

mod game;

use std::{
    env, fs,
    path::{Path, PathBuf},
    thread,
};

use anygenesis_abi::{AgEntry, RAM_SIZE, ram_index};
use anygenesis_rom::{Rom, Sha1Digest};
use anygenesis_runtime::{Access, Exit, Miss, Options, RUN_STACK, Runtime, Vdp, vdp::Frame};
use anyhow::{Context, Result, anyhow, bail};

use crate::game::CompiledGame;

const ROM_VAR: &str = "ANYGENESIS_ROM";
const USAGE: &str = "usage: anygenesis [<rom.bin> [<frames>]] [--dump <frame>[,<frame>...]] \
                     (the ROM may come from ANYGENESIS_ROM)";
/// Ten seconds of NTSC frames.
const DEFAULT_FRAMES: u64 = 600;
/// The work RAM byte the report shows; Sonic 1 keeps its game mode there.
const WATCHED_BYTE: u32 = 0xFF_F600;
/// Where `--dump` saves frames, under the working directory.
const DUMP_FOLDER: &str = "dumps";

fn main() -> Result<()> {
    let Some(game) = game::compiled() else {
        bail!("built without a game: set GAME and {ROM_VAR}, then rebuild");
    };
    let arguments = Arguments::parse()?;
    let rom = load_rom(&game, &arguments.rom)?;
    println!("game {}, ROM SHA-1 {}", game.name, game.sha1);
    println!(
        "{} functions, {} table rows, {} vector handlers",
        game.functions,
        game.table.len(),
        game.vector_handlers.len()
    );
    let report = run_from_reset(game.table, rom, arguments.frames, &arguments.dumps)?;
    report.print();
    for (frame, picture) in &report.pictures {
        let path = dump(game.name, *frame, picture, Path::new(DUMP_FOLDER))?;
        println!("frame {frame} saved to {}", path.display());
    }
    Ok(())
}

/// What the command line asks for.
#[derive(Debug)]
struct Arguments {
    /// The first argument, else `ANYGENESIS_ROM`.
    rom: PathBuf,
    /// The second argument, else ten seconds' worth.
    frames: u64,
    /// Frames to save, each from 1 to `frames`, in order.
    dumps: Vec<u64>,
}

impl Arguments {
    fn parse() -> Result<Self> {
        let mut positional = Vec::new();
        let mut dumps = Vec::new();
        let mut arguments = env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            if argument != "--dump" {
                positional.push(argument);
                continue;
            }
            let list = arguments.next().context(USAGE)?;
            let list = list
                .to_str()
                .with_context(|| format!("{list:?} is not frames"))?;
            for item in list.split(',') {
                dumps.push(
                    item.parse::<u64>()
                        .with_context(|| format!("{item:?} is not a frame\n{USAGE}"))?,
                );
            }
        }
        let mut positional = positional.into_iter();
        let rom = positional
            .next()
            .or_else(|| env::var_os(ROM_VAR))
            .map(PathBuf::from)
            .context(USAGE)?;
        let frames = match positional.next() {
            Some(argument) => {
                let argument = argument.to_string_lossy();
                argument
                    .parse()
                    .with_context(|| format!("{argument:?} is not a frame count\n{USAGE}"))?
            }
            None => DEFAULT_FRAMES,
        };
        if positional.next().is_some() {
            bail!(USAGE);
        }
        if let Some(frame) = dumps.iter().find(|&&frame| frame == 0 || frame > frames) {
            bail!("frame {frame} is not among the {frames} the run draws");
        }
        dumps.sort_unstable();
        dumps.dedup();
        Ok(Self { rom, frames, dumps })
    }
}

/// Saves `picture`, frame `frame` of `game`, as a BMP file in `folder`; returns its path.
fn dump(game: &str, frame: u64, picture: &Frame, folder: &Path) -> Result<PathBuf> {
    fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    let path = folder.join(format!("{game}-{frame:05}.bmp"));
    fs::write(&path, picture.to_bmp()).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
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
    misses: Vec<(u32, Miss)>,
    accesses: Vec<(Access, u64)>,
    unwinds: u64,
    tmss_unlocked: bool,
    /// Z80 RAM bytes that are not zero.
    z80_bytes: usize,
    vdp: Vdp,
    /// Work RAM at the end.
    ram: Box<[u8; RAM_SIZE]>,
    /// The frames asked for, by number, as drawn when they ended.
    pictures: Vec<(u64, Frame)>,
}

/// SHA-1s of the VDP's memories and registers, words in big-endian order, for comparing runs.
fn vdp_hashes(vdp: &Vdp) -> [(&'static str, Sha1Digest); 4] {
    let words = |words: &[u16]| -> Vec<u8> { words.iter().flat_map(|w| w.to_be_bytes()).collect() };
    [
        ("VRAM", Sha1Digest::of(vdp.vram())),
        ("CRAM", Sha1Digest::of(&words(vdp.cram()))),
        ("VSRAM", Sha1Digest::of(&words(vdp.vsram()))),
        ("registers", Sha1Digest::of(vdp.registers())),
    ]
}

impl Report {
    /// The work RAM byte the report shows.
    fn watched(&self) -> u8 {
        self.ram[ram_index(WATCHED_BYTE)]
    }

    fn print(&self) {
        println!("reset: SSP ${:08X}, PC ${:06X}", self.ssp, self.reset_pc);
        println!(
            "run ended: {} after {} cycles, {} frames",
            self.exit, self.cycles, self.frames
        );
        println!(
            "interrupts taken: {} VBlank (level 6), {} HBlank (level 4); SR ${:04X}, RAM \
             ${WATCHED_BYTE:06X} = ${:02X}",
            self.vblanks,
            self.hblanks,
            self.sr,
            self.watched()
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
        let counts = self.vdp.counts();
        println!(
            "VDP: {} register writes, {} command words, {} data writes, {} data reads, {} \
             status reads, {} PSG writes",
            counts.register_writes,
            counts.command_words,
            counts.data_writes,
            counts.data_reads,
            counts.status_reads,
            counts.psg_writes
        );
        println!(
            "VDP DMA: {} transfers ({} stall cycles), {} fills, {} copies",
            counts.transfers, counts.stall_cycles, counts.fills, counts.copies
        );
        for (name, hash) in vdp_hashes(&self.vdp) {
            println!("VDP {name} SHA-1 {hash}");
        }
        let registers: Vec<String> = self
            .vdp
            .registers()
            .iter()
            .map(|value| format!("{value:02X}"))
            .collect();
        println!("VDP registers 0-23: {}", registers.join(" "));
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
/// ended, or the run stops earlier at a wait nothing can end, keeping the pictures of the
/// frames in `dumps` (ascending, none past `frames`).
fn run_from_reset(
    table: &'static [AgEntry],
    rom: Rom,
    frames: u64,
    dumps: &[u64],
) -> Result<Report> {
    let dumps = dumps.to_vec();
    let worker = thread::Builder::new()
        .name("68000".into())
        .stack_size(RUN_STACK)
        .spawn(move || {
            let options = Options {
                stop_when_idle: true,
                ..Options::default()
            };
            let mut runtime = Runtime::new(rom, table, Box::<Vdp>::default(), options);
            let ssp = runtime.state().a[7];
            let reset_pc = runtime.state().pc;
            let (exit, pictures) = run_frames(&mut runtime, frames, &dumps);
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
                vdp: runtime.hardware::<Vdp>().cloned().unwrap_or_default(),
                ram: Box::new(state.ram),
                pictures,
            }
        })
        .context("starting the 68000 thread")?;
    worker
        .join()
        .map_err(|_| anyhow!("the 68000 thread panicked"))
}

/// Runs until `frames` frames have ended, stopping at each frame in `dumps` (ascending, none
/// past `frames`) to keep its picture.
fn run_frames(runtime: &mut Runtime, frames: u64, dumps: &[u64]) -> (Exit, Vec<(u64, Frame)>) {
    let mut pictures = Vec::new();
    for &target in dumps {
        let exit = runtime.run_frames(target - runtime.frames());
        if !matches!(exit, Exit::Frames { .. }) {
            return (exit, pictures);
        }
        if let Some(vdp) = runtime.hardware::<Vdp>() {
            pictures.push((target, vdp.frame().clone()));
        }
        if target == frames {
            return (exit, pictures);
        }
    }
    (runtime.run_frames(frames - runtime.frames()), pictures)
}

#[cfg(test)]
mod tests {
    use anygenesis_runtime::vdp::CRAM_MASK;

    use super::*;

    /// Sonic 1's palette buffer in work RAM, which its VBlank handler copies to CRAM by DMA.
    const PALETTE: u32 = 0xFF_FB00;
    /// Sonic 1's VDP setup table in ROM (REV00), one `0x8RVV` word per register 0-18.
    const VDP_SETUP_TABLE: usize = 0x129E;
    /// Sonic 1's game mode byte on the title screen.
    const TITLE_MODE: u8 = 0x04;
    /// The VDP's memories and registers after 600 frames, as Mehran signed them off
    /// (M3.3); they change whenever DMA, timing or the translated code's cycle counts do.
    const HASHES_AT_600: [(&str, &str); 4] = [
        ("VRAM", "4e3ebe64e3818005773b55531e85b615d092b8e9"),
        ("CRAM", "2ab1aba063597f9054b94bbf379bda288be40369"),
        ("VSRAM", "9faab1c6fb9abfc6d5a3c2a3d3149f0f3f7e4f4d"),
        ("registers", "6f90e45454834c6814184810aa3cd4d03cc6e8c4"),
    ];
    /// A frame showing the SEGA screen.
    const SEGA_FRAME: u64 = 240;
    /// SHA-1s of the BMP files of the SEGA frame and of frame 600 (the title screen), as
    /// Mehran signed the pictures off (M3.4); they change whenever the hashes above or the
    /// renderer do.
    const PICTURES: [(u64, &str); 2] = [
        (SEGA_FRAME, "87e9a8f99cee2471fa47e8d853165035cfbd9d39"),
        (DEFAULT_FRAMES, "55afe5b4bed5fceb13d7a21d615ca0df067d30ea"),
    ];

    /// The registers the title screen should leave: the setup table, read from the ROM,
    /// with what changes on top: the title screen turns the display on and sets backdrop
    /// palette 2 and line horizontal scroll, and the VBlank handler writes the horizontal
    /// interrupt line the setup code keeps in work RAM (223).
    fn title_registers(rom: &Rom) -> [u8; 19] {
        let table = &rom.bytes()[VDP_SETUP_TABLE..VDP_SETUP_TABLE + 2 * 19];
        let mut registers = [0; 19];
        for &[register, value] in table.as_chunks::<2>().0 {
            assert_eq!(register & 0xE0, 0x80, "a register write word");
            registers[usize::from(register & 0x1F)] = value;
        }
        registers[1] |= 0x40;
        registers[7] = 0x20;
        registers[10] = 223;
        registers[11] = 0x03;
        registers
    }

    /// Sonic 1 from reset for ten seconds of frames on the VDP, with interrupts and DMA,
    /// saving the SEGA and title screen pictures to `dumps/` at the workspace root.
    #[test]
    #[ignore = "needs a GAME=sonic1 build and ANYGENESIS_ROM"]
    fn test_sonic1_runs_600_frames_without_misses() {
        let game = game::compiled().expect("the game crate was built with GAME set");
        let path = env::var_os(ROM_VAR).expect("ANYGENESIS_ROM is set");
        let rom = load_rom(&game, Path::new(&path)).expect("the ROM matches the build");
        let registers = title_registers(&rom);
        let dumps = PICTURES.map(|(frame, _)| frame);
        let report =
            run_from_reset(game.table, rom, DEFAULT_FRAMES, &dumps).expect("the run finishes");
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dumps");
        for (frame, picture) in &report.pictures {
            dump(game.name, *frame, picture, &folder).expect("the picture is saved");
        }
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
        assert!(report.z80_bytes > 0);
        assert_eq!(report.watched(), TITLE_MODE);

        // The title screen has faded in, so the last VBlank's DMA left CRAM equal to the
        // palette buffer.
        let palette: Vec<u16> = (0..64)
            .map(|index| {
                let at = ram_index(PALETTE) + 2 * index;
                u16::from_be_bytes([report.ram[at], report.ram[at + 1]]) & CRAM_MASK
            })
            .collect();
        assert_eq!(report.vdp.cram()[..], palette[..]);
        assert!(palette.iter().any(|&colour| colour != 0));
        assert_eq!(report.vdp.registers()[..19], registers[..]);

        let hashes: Vec<(&str, String)> = vdp_hashes(&report.vdp)
            .into_iter()
            .map(|(name, hash)| (name, hash.to_string()))
            .collect();
        let expected: Vec<(&str, String)> = HASHES_AT_600
            .into_iter()
            .map(|(name, hash)| (name, hash.to_owned()))
            .collect();
        assert_eq!(hashes, expected);

        let pictures: Vec<(u64, String)> = report
            .pictures
            .iter()
            .map(|(frame, picture)| (*frame, Sha1Digest::of(&picture.to_bmp()).to_string()))
            .collect();
        let expected: Vec<(u64, String)> = PICTURES
            .into_iter()
            .map(|(frame, hash)| (frame, hash.to_owned()))
            .collect();
        assert_eq!(pictures, expected);
    }
}
