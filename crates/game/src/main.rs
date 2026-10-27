//! The recompiled game: translated 68000 code linked against the runtime.
//!
//! The binary loads and checks the ROM, checks the translated program's address table and
//! runs from reset on the VDP, which keeps its memories, runs DMA, raises the vertical and
//! horizontal interrupts and draws the picture. Given only the ROM it runs in a window with
//! the keyboard and gamepad as pad 1 until the window closes. Given a frame count, or any of
//! the options below, it runs headless for that many frames (ten seconds' worth by default)
//! instead. Either way it reports where the run ended, what the VDP holds and what the
//! runtime logged on the way, and headless runs save the frames asked for with `--dump` as
//! BMP files in `dumps/` under the working directory.
//!
//! For the frame diff, `--state <folder>` saves the machine state at every vertical
//! interrupt handler entry in the frame diff's dump layout, on a console without TMSS like
//! the reference emulator's, and `--input <file>` replays pad input from the frame diff's
//! input format at the same points.

mod game;

use std::{
    cell::RefCell,
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    thread,
};

use anygenesis_abi::{AgState, RAM_SIZE, ram_index};
use anygenesis_framediff::{Dump, Input, Region, dump};
use anygenesis_frontend::Config;
use anygenesis_rom::{Rom, Sha1Digest};
use anygenesis_runtime::{
    Access, Buttons, Exit, Hardware, InterruptStop, Miss, Options, RUN_STACK, Runtime, Vdp,
    io::{VERSION_NO_EXPANSION, VERSION_OVERSEAS},
    vdp::Frame,
};
use anyhow::{Context, Result, anyhow, bail};

use crate::game::CompiledGame;

const ROM_VAR: &str = "ANYGENESIS_ROM";
const USAGE: &str = "usage: anygenesis [<rom.bin> [<frames>]] [--dump <frame>[,<frame>...]] \
                     [--state <folder>] [--input <file>] (the ROM may come from \
                     ANYGENESIS_ROM; with no frames and no options it runs in a window)";
/// Window pixels per picture pixel when the window opens.
const WINDOW_SCALE: u32 = 3;
/// The vertical interrupt's level: the frame diff stops at its handler's entry.
const VBLANK_LEVEL: u8 = 6;
/// The version register of the reference emulator's console: overseas NTSC, no Mega CD,
/// hardware version 0 (no TMSS).
const REFERENCE_VERSION: u8 = VERSION_OVERSEAS | VERSION_NO_EXPANSION;
/// Ten seconds of NTSC frames.
const DEFAULT_FRAMES: u64 = 600;
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
    let Some(frames) = arguments.frames else {
        run_in_window(game, rom)?.print();
        return Ok(());
    };
    let trace = Trace {
        folder: arguments.state.clone(),
        input: arguments.input.as_deref().map(Input::load).transpose()?,
    };
    if let Some(folder) = &trace.folder {
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    }
    let report = run_from_reset(game, rom, frames, &arguments.dumps, trace)?;
    report.print();
    if let Some(folder) = &arguments.state {
        println!(
            "state saved at {} vertical interrupts in {} (version register \
             ${REFERENCE_VERSION:02X})",
            report.vblanks,
            folder.display()
        );
    }
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
    /// Frames to run headless: the second argument, else ten seconds' worth when another
    /// option asks for a headless run; `None` runs in a window.
    frames: Option<u64>,
    /// Frames to save, each from 1 to `frames`, in order.
    dumps: Vec<u64>,
    /// Where to save the state at every vertical interrupt.
    state: Option<PathBuf>,
    /// The pad input to replay.
    input: Option<PathBuf>,
}

impl Arguments {
    fn parse() -> Result<Self> {
        let mut positional = Vec::new();
        let mut dumps = Vec::new();
        let (mut state, mut input) = (None, None);
        let mut arguments = env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            let path = match argument.to_str() {
                Some("--state") => Some(&mut state),
                Some("--input") => Some(&mut input),
                _ => None,
            };
            if let Some(path) = path {
                let value = arguments.next().context(USAGE)?;
                if path.replace(PathBuf::from(value)).is_some() {
                    bail!(USAGE);
                }
                continue;
            }
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
                Some(
                    argument
                        .parse()
                        .with_context(|| format!("{argument:?} is not a frame count\n{USAGE}"))?,
                )
            }
            None if dumps.is_empty() && state.is_none() && input.is_none() => None,
            None => Some(DEFAULT_FRAMES),
        };
        if positional.next().is_some() {
            bail!(USAGE);
        }
        let last = frames.unwrap_or(0);
        if let Some(frame) = dumps.iter().find(|&&frame| frame == 0 || frame > last) {
            bail!("frame {frame} is not among the {last} the run draws");
        }
        dumps.sort_unstable();
        dumps.dedup();
        Ok(Self {
            rom,
            frames,
            dumps,
            state,
            input,
        })
    }
}

/// Where the trace hook leaves its first error; the hook cannot return one.
type Failure = Rc<RefCell<Option<anyhow::Error>>>;

/// What a run does at every vertical interrupt handler entry for the frame diff.
#[derive(Debug, Default)]
struct Trace {
    /// Where to save the machine state.
    folder: Option<PathBuf>,
    /// The pad input to apply.
    input: Option<Input>,
}

impl Trace {
    fn is_empty(&self) -> bool {
        self.folder.is_none() && self.input.is_none()
    }

    /// Sets the hook on `runtime`; the first failure to save a stop lands in `failure`, and
    /// no stop is saved after it.
    fn hook(self, runtime: &mut Runtime, game: CompiledGame, failure: Failure) {
        runtime.set_interrupt_hook(VBLANK_LEVEL, move |stop| {
            let index = stop.taken();
            if let Some(folder) = &self.folder
                && failure.borrow().is_none()
                && let Err(error) = save_stop(folder, index, stop, &game)
            {
                *failure.borrow_mut() = Some(error);
            }
            if let Some(input) = &self.input {
                stop.set_pad(Buttons(input.at(index).0));
            }
        });
    }
}

/// Saves the state at stop `index` in the frame diff's layout, with a note of where the
/// interrupt came in.
fn save_stop(
    folder: &Path,
    index: u64,
    stop: &InterruptStop<'_>,
    game: &CompiledGame,
) -> Result<()> {
    let vdp = stop.hardware::<Vdp>().context("the run has no VDP")?;
    let state = stop.state();
    let mut dump = Dump::default();
    dump.region_mut(Region::Ram).copy_from_slice(&state.ram);
    dump.region_mut(Region::Vram).copy_from_slice(vdp.vram());
    dump.region_mut(Region::Cram)
        .copy_from_slice(&big_endian(vdp.cram()));
    dump.region_mut(Region::Vsram)
        .copy_from_slice(&big_endian(vdp.vsram()));
    dump.region_mut(Region::Registers)
        .copy_from_slice(vdp.registers());
    dump.write(folder, index)?;
    dump::write_note(folder, index, &stop_note(index, stop, vdp, game))?;
    Ok(())
}

/// What the dump does not hold: when the stop came, and the code the interrupt came in at.
fn stop_note(index: u64, stop: &InterruptStop<'_>, vdp: &Vdp, game: &CompiledGame) -> String {
    let state = stop.state();
    let sp = state.a[7];
    let stacked_sr = word(state, sp);
    let interrupted = u32::from(word(state, sp + 2)) << 16 | u32::from(word(state, sp + 4));
    let function = game
        .function_at(interrupted)
        .map_or_else(|| "none".to_owned(), |entry| format!("${entry:06X}"));
    format!(
        "stop {index}\nframe {}\nline {}\nhv ${:04X}\nclock {}\nhandler ${:06X}\nsp ${sp:08X}\n\
         interrupted ${interrupted:06X} with sr ${stacked_sr:04X}\nfunction {function}\n",
        Hardware::frames(vdp),
        vdp.timing().v_counter(),
        vdp.timing().hv(),
        vdp.timing().clock(),
        stop.handler(),
    )
}

/// The word at `address` in work RAM.
fn word(state: &AgState, address: u32) -> u16 {
    u16::from_be_bytes([
        state.ram[ram_index(address)],
        state.ram[ram_index(address + 1)],
    ])
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
    /// Interrupts taken late, at a native check point (see `Runtime::late_interrupts`).
    late: u64,
    /// Blocks the hybrid path ran through the interpreter.
    hybrids: u64,
    /// The status register's system byte at the end.
    sr: u16,
    misses: Vec<(u32, Miss)>,
    accesses: Vec<(Access, u64)>,
    unwinds: u64,
    tmss_unlocked: bool,
    /// Z80 RAM bytes that are not zero.
    z80_bytes: usize,
    /// The work RAM byte the report shows, when the game's config names one.
    watched: Option<u32>,
    vdp: Vdp,
    /// Work RAM at the end.
    ram: Box<[u8; RAM_SIZE]>,
    /// The frames asked for, by number, as drawn when they ended.
    pictures: Vec<(u64, Frame)>,
}

/// SHA-1s of the VDP's memories and registers, words in big-endian order, for comparing runs.
fn vdp_hashes(vdp: &Vdp) -> [(&'static str, Sha1Digest); 4] {
    [
        ("VRAM", Sha1Digest::of(vdp.vram())),
        ("CRAM", Sha1Digest::of(&big_endian(vdp.cram()))),
        ("VSRAM", Sha1Digest::of(&big_endian(vdp.vsram()))),
        ("registers", Sha1Digest::of(vdp.registers())),
    ]
}

/// The bytes of `words` in the 68000's order.
fn big_endian(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

impl Report {
    /// What `runtime` holds after a run of `game` that ended with `exit`, with `pictures`
    /// kept on the way.
    fn new(
        runtime: &Runtime,
        game: &CompiledGame,
        exit: Exit,
        pictures: Vec<(u64, Frame)>,
    ) -> Self {
        let vectors = runtime.rom().vectors();
        let state = runtime.state();
        Self {
            ssp: vectors.initial_ssp(),
            reset_pc: vectors.reset_pc() & 0xFF_FFFF,
            exit,
            cycles: runtime.cycles(),
            frames: runtime.frames(),
            vblanks: runtime.interrupts(6),
            hblanks: runtime.interrupts(4),
            late: runtime.late_interrupts(),
            hybrids: runtime.hybrid_blocks(),
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
            watched: game.report.map(|report| report.watched),
            vdp: runtime.hardware::<Vdp>().cloned().unwrap_or_default(),
            ram: Box::new(state.ram),
            pictures,
        }
    }

    /// The work RAM byte the report shows, with its address.
    fn watched(&self) -> Option<(u32, u8)> {
        self.watched
            .map(|address| (address, self.ram[ram_index(address)]))
    }

    fn print(&self) {
        println!("reset: SSP ${:08X}, PC ${:06X}", self.ssp, self.reset_pc);
        println!(
            "run ended: {} after {} cycles, {} frames",
            self.exit, self.cycles, self.frames
        );
        let watched = self.watched().map_or_else(String::new, |(address, value)| {
            format!(", RAM ${address:06X} = ${value:02X}")
        });
        println!(
            "interrupts taken: {} VBlank (level 6), {} HBlank (level 4); SR ${:04X}{watched}",
            self.vblanks, self.hblanks, self.sr
        );
        println!(
            "interrupt delivery: {} late, {} blocks interpreted on the hybrid path",
            self.late, self.hybrids
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
/// frames in `dumps` (ascending, none past `frames`) and doing what `trace` asks at every
/// vertical interrupt. A run that saves state runs on the reference emulator's console.
fn run_from_reset(
    game: CompiledGame,
    rom: Rom,
    frames: u64,
    dumps: &[u64],
    trace: Trace,
) -> Result<Report> {
    let dumps = dumps.to_vec();
    let worker = thread::Builder::new()
        .name("68000".into())
        .stack_size(RUN_STACK)
        .spawn(move || {
            let reference = trace.folder.is_some();
            let mut runtime = start(rom, game, reference);
            let failure = Failure::default();
            if !trace.is_empty() {
                trace.hook(&mut runtime, game, Rc::clone(&failure));
            }
            let (exit, pictures) = run_frames(&mut runtime, frames, &dumps);
            if let Some(error) = failure.take() {
                return Err(error.context("saving the state at a vertical interrupt"));
            }
            Ok(Report::new(&runtime, &game, exit, pictures))
        })
        .context("starting the 68000 thread")?;
    worker
        .join()
        .map_err(|_| anyhow!("the 68000 thread panicked"))?
}

/// Runs from reset in a window until it closes, or the run stops at a wait nothing can end.
fn run_in_window(game: CompiledGame, rom: Rom) -> Result<Report> {
    let config = Config {
        title: format!("AnyGenesis - {}", game.name),
        scale: WINDOW_SCALE,
    };
    let report = anygenesis_frontend::run(
        &config,
        move || start(rom, game, false),
        move |runtime, exit| Report::new(runtime, &game, exit, Vec::new()),
    )?;
    Ok(report)
}

/// The runtime for `game` at reset on the VDP, ending a run at a wait nothing can end; on
/// the reference emulator's console when `reference` is set.
fn start(rom: Rom, game: CompiledGame, reference: bool) -> Runtime {
    let defaults = Options::default();
    let options = Options {
        stop_when_idle: true,
        version: if reference {
            REFERENCE_VERSION
        } else {
            defaults.version
        },
        ..defaults
    };
    Runtime::new(rom, game.table, Box::<Vdp>::default(), options)
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
    use crate::game::ReportConfig;

    /// The VDP's memories and registers after 600 frames, as Mehran signed them off
    /// (M3.3, VRAM again at v0.2 M1.1); they change whenever DMA, timing or the translated
    /// code's cycle counts do.
    const HASHES_AT_600: [(&str, &str); 4] = [
        ("VRAM", "af90156571d5d2313cb701d0568ffa7552674efa"),
        ("CRAM", "2ab1aba063597f9054b94bbf379bda288be40369"),
        ("VSRAM", "9faab1c6fb9abfc6d5a3c2a3d3149f0f3f7e4f4d"),
        ("registers", "6f90e45454834c6814184810aa3cd4d03cc6e8c4"),
    ];
    /// A frame showing the SEGA screen.
    const SEGA_FRAME: u64 = 240;
    /// SHA-1s of the BMP files of the SEGA frame and of frame 600 (the title screen), as
    /// Mehran signed the pictures off (M3.4, frame 600 again at v0.2 M1.1); they change
    /// whenever the hashes above or the renderer do.
    const PICTURES: [(u64, &str); 2] = [
        (SEGA_FRAME, "87e9a8f99cee2471fa47e8d853165035cfbd9d39"),
        (DEFAULT_FRAMES, "58274d550398349f85b1745ff30870605aa13f11"),
    ];

    /// The registers the title screen should leave: the setup table at `setup`, read from
    /// the ROM, with what changes on top: the title screen turns the display on and sets
    /// backdrop palette 2 and line horizontal scroll, and the VBlank handler writes the
    /// horizontal interrupt line the setup code keeps in work RAM (223).
    fn title_registers(rom: &Rom, setup: u32) -> [u8; 19] {
        let setup = setup as usize;
        let table = &rom.bytes()[setup..setup + 2 * 19];
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

    /// The translated game and the ROM it came from.
    fn sonic1() -> (CompiledGame, Rom) {
        let game = game::compiled().expect("the game crate was built with GAME set");
        let path = env::var_os(ROM_VAR).expect("ANYGENESIS_ROM is set");
        let rom = load_rom(&game, Path::new(&path)).expect("the ROM matches the build");
        (game, rom)
    }

    /// The config's `[report]`, which the checks read.
    fn report_config(game: &CompiledGame) -> ReportConfig {
        game.report
            .expect("games/sonic1.toml has a [report] section")
    }

    /// The watched byte's value `name` from `[report.values]`.
    fn value(config: &ReportConfig, name: &str) -> u8 {
        config
            .values
            .iter()
            .find(|&&(key, _)| key == name)
            .map(|&(_, value)| value)
            .unwrap_or_else(|| panic!("[report.values] has no {name}"))
    }

    /// Sonic 1 from reset for ten seconds of frames on the VDP, with interrupts and DMA,
    /// saving the SEGA and title screen pictures to `dumps/` at the workspace root.
    #[test]
    #[ignore = "needs a GAME=sonic1 build and ANYGENESIS_ROM"]
    fn test_sonic1_runs_600_frames_without_misses() {
        let (game, rom) = sonic1();
        let config = report_config(&game);
        let palette_copy = config.palette.expect("[report] has palette");
        let setup = config.vdp_setup.expect("[report] has vdp_setup");
        let registers = title_registers(&rom, setup);
        let dumps = PICTURES.map(|(frame, _)| frame);
        // An empty input still sets the hook at every vertical interrupt, which must change
        // nothing below.
        let trace = Trace {
            folder: None,
            input: Some(Input::default()),
        };
        let report =
            run_from_reset(game, rom, DEFAULT_FRAMES, &dumps, trace).expect("the run finishes");
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
        assert_eq!(
            report.watched(),
            Some((config.watched, value(&config, "title")))
        );

        // The title screen has faded in, so the last VBlank's DMA left CRAM equal to the
        // palette buffer.
        let palette: Vec<u16> = (0..64)
            .map(|index| {
                let at = ram_index(palette_copy) + 2 * index;
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

    /// The machine where a run ended, for comparing two runs.
    #[derive(Debug, PartialEq, Eq)]
    struct Outcome {
        exit: Exit,
        cycles: u64,
        vblanks: u64,
        d: [u32; 8],
        a: [u32; 8],
        sr: u16,
        /// SHA-1s of work RAM, then of the VDP's memories and registers.
        hashes: Vec<String>,
    }

    /// Runs `frames` frames from reset with budgets of at most `slice` cycles, on a thread
    /// with room for the native stack. Also returns the late interrupts and hybrid blocks.
    fn outcome(game: CompiledGame, rom: Rom, frames: u64, slice: i32) -> (Outcome, u64, u64) {
        let worker = thread::Builder::new()
            .name("68000".into())
            .stack_size(RUN_STACK)
            .spawn(move || {
                let options = Options {
                    slice,
                    ..Options::default()
                };
                let mut runtime = Runtime::new(rom, game.table, Box::<Vdp>::default(), options);
                let exit = runtime.run_frames(frames);
                let state = runtime.state();
                let vdp = runtime.hardware::<Vdp>().expect("the hardware is the VDP");
                let mut hashes = vec![Sha1Digest::of(&state.ram).to_string()];
                hashes.extend(vdp_hashes(vdp).map(|(_, hash)| hash.to_string()));
                let outcome = Outcome {
                    exit,
                    cycles: runtime.cycles(),
                    vblanks: runtime.interrupts(6),
                    d: state.d,
                    a: state.a,
                    sr: state.sr,
                    hashes,
                };
                (outcome, runtime.late_interrupts(), runtime.hybrid_blocks())
            })
            .expect("the 68000 thread starts");
        worker.join().expect("the run finishes")
    }

    /// The hybrid path against translated code: with one-cycle budgets every budget ends
    /// inside its block, so every block runs through the interpreter, and the machine must
    /// end as it does with native blocks. Only an interrupt native code took late could tell
    /// them apart.
    #[test]
    #[ignore = "needs a GAME=sonic1 build and ANYGENESIS_ROM"]
    fn test_sonic1_interpreted_blocks_end_where_native_ones_do() {
        let (game, rom) = sonic1();
        let slice = Options::default().slice;
        let (native, late, hybrids) = outcome(game, rom.clone(), DEFAULT_FRAMES, slice);
        let (interpreted, _, every) = outcome(game, rom, DEFAULT_FRAMES, 1);
        println!("native: {late} late, {hybrids} hybrid blocks; one-cycle budgets: {every}");
        assert_eq!(late, 0);
        assert_eq!(native, interpreted);
    }

    /// Start held for ten vertical interrupts on the idle title screen (stop 298 ends frame
    /// 600), in the frame diff's input format.
    const START_PRESS: &str = "330 S\n340 -\n";
    /// Frames the Start run lasts.
    const START_FRAMES: u64 = 900;

    /// Pressing Start on the title screen leaves it for the first level.
    #[test]
    #[ignore = "needs a GAME=sonic1 build and ANYGENESIS_ROM"]
    fn test_sonic1_title_screen_accepts_start() {
        let (game, rom) = sonic1();
        let config = report_config(&game);
        let trace = Trace {
            folder: None,
            input: Some(Input::parse(START_PRESS).expect("the input is valid")),
        };
        let report = run_from_reset(game, rom, START_FRAMES, &[], trace).expect("the run finishes");
        report.print();
        assert!(
            matches!(report.exit, Exit::Frames { .. }),
            "{}",
            report.exit
        );
        assert_eq!(report.frames, START_FRAMES);
        assert_eq!(report.unwinds, 0);
        assert_eq!(
            report.watched(),
            Some((config.watched, value(&config, "level")))
        );
    }
}
