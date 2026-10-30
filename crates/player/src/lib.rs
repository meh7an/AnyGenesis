//! The player: runs a translated game linked into the same program against the runtime.
//!
//! A game program carries its whole ROM image. The player checks that image against the
//! SHA-1 its build recorded, checks the translated program's address table and runs from
//! reset on the VDP, which keeps its memories, runs DMA, raises the vertical and horizontal
//! interrupts and draws the picture. With no arguments it runs in a window with the
//! keyboard and gamepad as pad 1 until the window closes; the buttons reach the game at every
//! vertical interrupt handler entry, as they last were there. Given a frame count, or
//! `--dump`, `--input` or `--state` without `--record`, it runs headless for that many
//! frames (ten seconds' worth by default) instead. Either way it reports where the run
//! ended, what the VDP holds and what the runtime logged on the way, and headless runs save
//! the frames asked for with `--dump` as BMP files in `dumps/` under the working directory.
//!
//! For the frame diff, `--state <folder>` saves the machine state at every vertical
//! interrupt handler entry in the frame diff's dump layout, `--input <file>` replays pad
//! input from the frame diff's input format at the same points, and `--record <file>`
//! (window only, `--state` may go with it) writes the buttons the window applied there in
//! that format. Runs with any of the three use a console without TMSS like the reference
//! emulator's, so a recording replays to the same machine headless and in the reference.
//! `--sound-log <file>` (headless) writes every write to the sound chips, from either CPU,
//! one per line with its master clock.
//!
//! The window reads its key and gamepad bindings, hotkeys (fullscreen, scale, pause, reset,
//! quit) and start-up settings from `--controls <file>`, else from `controls.toml` in SDL's
//! per-user folder if it is there, else uses the defaults. It remembers the picture scaling
//! the scale hotkey picks in `settings.toml` in that folder. A reset builds the run afresh, as
//! at start; a run that records refuses it, so a recording is one run from power-on. Pauses
//! are free: they fall between vertical interrupts, where nothing is recorded.

mod game;

use std::{
    cell::RefCell,
    env,
    fmt::Write as _,
    fs::{self, File},
    path::{Path, PathBuf},
    rc::Rc,
    thread,
};

use anygenesis_abi::{AgState, RAM_SIZE, ram_index};
use anygenesis_framediff::{Dump, Input, Recorder, Region, dump};
use anygenesis_frontend::{
    Config, Controls, Pad, Settings, controls,
    mix::{self, Mixer},
    settings,
};
use anygenesis_rom::{Rom, Sha1Digest};
use anygenesis_runtime::{
    Access, Buttons, Chip, Cpu, Exit, Hardware, InterruptStop, Miss, Options, RUN_STACK, Runtime,
    SoundCounts, SoundWrite, Vdp,
    io::{VERSION_NO_EXPANSION, VERSION_OVERSEAS},
    vdp::{Frame, render::MAX_WIDTH},
};
use anyhow::{Context, Result, anyhow, bail};

pub use anygenesis_runtime::has_interpreter;

pub use crate::game::Game;

const USAGE: &str = "usage: <game> [<frames>] [--dump <frame>[,<frame>...]] [--state <folder>] \
                     [--input <file>] [--sound-log <file>] [--wav <file>] [--record <file>] \
                     [--controls <file>] (the game carries its own ROM image; it runs in a window with no \
                     frames and no options but --controls, or with --record and at most \
                     --state)";
/// Window pixels per picture pixel when the window opens.
const WINDOW_SCALE: u32 = 3;
/// The vertical interrupt's level: the frame diff stops at its handler's entry.
const VBLANK_LEVEL: u8 = 6;
/// The version register of the reference emulator's console: overseas NTSC, no Mega CD,
/// hardware version 0 (no TMSS).
const REFERENCE_VERSION: u8 = VERSION_OVERSEAS | VERSION_NO_EXPANSION;
/// Ten seconds of NTSC frames.
pub const DEFAULT_FRAMES: u64 = 600;
/// Where `--dump` saves frames, under the working directory.
const DUMP_FOLDER: &str = "dumps";

/// Runs `game` as the command line asks.
///
/// # Errors
/// A command line it cannot follow, an image or table that fails its checks, a run that
/// fails at a stop or ends at an address with no native code, or files it cannot write.
pub fn main(game: Game) -> Result<()> {
    let arguments = Arguments::parse()?;
    let rom = game.rom()?;
    println!("game {}, ROM SHA-1 {}", game.name, game.sha1);
    println!(
        "{} functions, {} table rows, {} vector handlers",
        game.function_entries.len(),
        game.table.len(),
        game.vector_handlers.len()
    );
    let reference =
        arguments.state.is_some() || arguments.input.is_some() || arguments.record.is_some();
    if let Some(folder) = &arguments.state {
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    }
    let pad = match arguments.input.as_deref() {
        Some(path) => PadSource::Replay(Input::load(path)?),
        None => PadSource::Untouched,
    };
    let record = arguments
        .record
        .as_deref()
        .map(|path| {
            File::create(path)
                .map(Recorder::new)
                .with_context(|| format!("creating {}", path.display()))
        })
        .transpose()?;
    let trace = Trace {
        folder: arguments.state.clone(),
        pad,
        record,
        sound: arguments.sound_log.is_some() || arguments.wav.is_some(),
    };
    let report = match arguments.frames {
        Some(frames) => run_from_reset(game, rom, frames, &arguments.dumps, trace, reference)?,
        None => {
            let controls = load_controls(arguments.controls.as_deref())?;
            run_in_window(game, rom, trace, reference, controls)?
        }
    };
    report.print();
    if let Some(folder) = &arguments.state {
        println!(
            "state saved at {} vertical interrupts in {} (version register \
             ${REFERENCE_VERSION:02X})",
            report.vblanks,
            folder.display()
        );
    }
    if let Some(path) = &arguments.record {
        println!(
            "input recorded at {} vertical interrupts to {} (version register \
             ${REFERENCE_VERSION:02X})",
            report.vblanks,
            path.display()
        );
    }
    for (frame, picture) in &report.pictures {
        let path = save_picture(*frame, picture, Path::new(DUMP_FOLDER))?;
        println!("frame {frame} saved to {}", path.display());
    }
    if let Some(path) = &arguments.sound_log {
        fs::write(path, sound_log(&report.sound_writes))
            .with_context(|| format!("writing {}", path.display()))?;
        println!(
            "{} sound writes saved to {}",
            report.sound_writes.len(),
            path.display()
        );
    }
    if let Some(path) = &arguments.wav {
        let samples = sound_samples(&report);
        fs::write(path, mix::wav(&samples))
            .with_context(|| format!("writing {}", path.display()))?;
        println!(
            "{} stereo samples at {} Hz saved to {}",
            samples.len() / 2,
            mix::SAMPLE_RATE,
            path.display()
        );
    }
    if let Exit::NoNativeCode { pc } = report.exit {
        bail!("the game ran into ${pc:06X}, which has no native code");
    }
    Ok(())
}

/// What the command line asks for.
#[derive(Debug)]
struct Arguments {
    /// Frames to run headless: the argument, else ten seconds' worth when another option
    /// asks for a headless run; `None` runs in a window.
    frames: Option<u64>,
    /// Frames to save, each from 1 to `frames`, in order.
    dumps: Vec<u64>,
    /// Where to save the state at every vertical interrupt.
    state: Option<PathBuf>,
    /// The pad input to replay.
    input: Option<PathBuf>,
    /// Where to record the window's pad input.
    record: Option<PathBuf>,
    /// The window's controls file, instead of the per-user one.
    controls: Option<PathBuf>,
    /// Where to save the sound writes.
    sound_log: Option<PathBuf>,
    /// Where to save the sound as a WAV file.
    wav: Option<PathBuf>,
}

impl Arguments {
    fn parse() -> Result<Self> {
        let mut positional = Vec::new();
        let mut dumps = Vec::new();
        let (mut state, mut input, mut record, mut controls) = (None, None, None, None);
        let (mut sound_log, mut wav) = (None, None);
        let mut arguments = env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            let path = match argument.to_str() {
                Some("--state") => Some(&mut state),
                Some("--input") => Some(&mut input),
                Some("--record") => Some(&mut record),
                Some("--controls") => Some(&mut controls),
                Some("--sound-log") => Some(&mut sound_log),
                Some("--wav") => Some(&mut wav),
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
        let frames = match positional.next() {
            Some(argument) => {
                let argument = argument.to_string_lossy();
                Some(argument.parse().with_context(|| {
                    format!(
                        "{argument:?} is not a frame count: this game carries its own ROM \
                         image and takes no ROM file\n{USAGE}"
                    )
                })?)
            }
            None if dumps.is_empty()
                && input.is_none()
                && sound_log.is_none()
                && wav.is_none()
                && (state.is_none() || record.is_some()) =>
            {
                None
            }
            None => Some(DEFAULT_FRAMES),
        };
        let window_only = record.is_some() || controls.is_some();
        if positional.next().is_some()
            || window_only && frames.is_some()
            || window_only && (sound_log.is_some() || wav.is_some())
        {
            bail!(USAGE);
        }
        let last = frames.unwrap_or(0);
        if let Some(frame) = dumps.iter().find(|&&frame| frame == 0 || frame > last) {
            bail!("frame {frame} is not among the {last} the run draws");
        }
        dumps.sort_unstable();
        dumps.dedup();
        Ok(Self {
            frames,
            dumps,
            state,
            input,
            record,
            controls,
            sound_log,
            wav,
        })
    }
}

/// Where the trace hook leaves its first error; the hook cannot return one.
pub type Failure = Rc<RefCell<Option<anyhow::Error>>>;

/// A recording the trace hook writes to, kept to end it once the run has.
pub type Recording = Rc<RefCell<Recorder<File>>>;

/// What a run does at every vertical interrupt handler entry: save the state for the frame
/// diff, apply the pad and record what it applied; and whether it keeps its sound writes.
#[derive(Debug, Default)]
pub struct Trace {
    /// Where to save the machine state.
    pub folder: Option<PathBuf>,
    pub pad: PadSource,
    /// Where to record the buttons applied.
    pub record: Option<Recorder<File>>,
    /// Keep every write to the sound chips, for the report (headless runs only): what the
    /// sound log and the WAV file are made from.
    pub sound: bool,
}

/// Where the pad's buttons at a stop come from.
#[derive(Debug, Default)]
pub enum PadSource {
    /// Nowhere: the pad stays as it is.
    #[default]
    Untouched,
    /// An input file.
    Replay(Input),
    /// The window, as it last read the keyboard and gamepad.
    Live(Pad),
}

impl Trace {
    fn is_empty(&self) -> bool {
        self.folder.is_none() && matches!(self.pad, PadSource::Untouched) && self.record.is_none()
    }

    /// Sets the hook on `runtime` and returns the recording, if any. The first failure to
    /// save a stop or record the pad lands in `failure`, and nothing is saved or recorded
    /// after it.
    pub fn hook(self, runtime: &mut Runtime, game: Game, failure: Failure) -> Option<Recording> {
        let Self {
            folder,
            pad,
            record,
            sound: _,
        } = self;
        let recording = record.map(|recorder| Rc::new(RefCell::new(recorder)));
        let recorder = recording.clone();
        runtime.set_interrupt_hook(VBLANK_LEVEL, move |stop| {
            let index = stop.taken();
            let buttons = match &pad {
                PadSource::Untouched => None,
                PadSource::Replay(input) => Some(Buttons(input.at(index).0)),
                PadSource::Live(pad) => Some(pad.buttons()),
            };
            if failure.borrow().is_none() {
                let saved = folder.as_deref().map_or(Ok(()), |folder| {
                    save_stop(folder, index, stop, &game).context("saving the state")
                });
                let done = saved.and_then(|()| match (&recorder, buttons) {
                    (Some(recorder), Some(buttons)) => recorder
                        .borrow_mut()
                        .record(index, anygenesis_framediff::Buttons(buttons.0))
                        .context("recording the pad"),
                    _ => Ok(()),
                });
                if let Err(error) = done {
                    failure.replace(Some(error));
                }
            }
            if let Some(buttons) = buttons {
                stop.set_pad(buttons);
            }
        });
        recording
    }
}

/// The report of a run that ended with `exit`, with `pictures` kept on the way, after ending
/// its recording.
///
/// # Errors
/// The first failure at a stop, or ending the recording.
pub fn conclude(
    runtime: &Runtime,
    exit: Exit,
    pictures: Vec<(u64, Frame)>,
    failure: &Failure,
    recording: Option<&Recording>,
) -> Result<Report> {
    if let Some(error) = failure.take() {
        return Err(error.context("at a vertical interrupt"));
    }
    if let Some(recording) = recording {
        recording
            .borrow_mut()
            .finish(runtime.interrupts(VBLANK_LEVEL), runtime.frames())
            .context("ending the recording")?;
    }
    Ok(Report::new(runtime, exit, pictures))
}

/// Saves the state at stop `index` in the frame diff's layout, with a note of where the
/// interrupt came in.
fn save_stop(folder: &Path, index: u64, stop: &InterruptStop<'_>, game: &Game) -> Result<()> {
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
fn stop_note(index: u64, stop: &InterruptStop<'_>, vdp: &Vdp, game: &Game) -> String {
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

/// The text of a sound log: a comment line, then one write per line: master clock, CPU,
/// chip (`ym1`, `ym2`, `psg`, or `ymreset` for the YM2612's reset line), YM2612 register
/// (`-` for the others) and value, in hex.
pub fn sound_log(writes: &[SoundWrite]) -> String {
    let mut text = String::from("# sound writes: master clock, CPU, chip, register, value\n");
    for write in writes {
        let cpu = match write.cpu {
            Cpu::M68000 => "68k",
            Cpu::Z80 => "z80",
        };
        let (chip, register) = match write.chip {
            Chip::Ym2612 { part, register } => (format!("ym{part}"), format!("{register:02X}")),
            Chip::Psg => ("psg".to_owned(), "-".to_owned()),
            Chip::Ym2612Reset => ("ymreset".to_owned(), "-".to_owned()),
        };
        // Writing to a String cannot fail.
        let _ = writeln!(
            text,
            "{} {cpu} {chip} {register} {:02X}",
            write.clock, write.value
        );
    }
    text
}

/// The sound of a run that kept its sound writes, from reset to where it ended: interleaved
/// stereo at [`mix::SAMPLE_RATE`].
pub fn sound_samples(report: &Report) -> Vec<f32> {
    let mut samples = Vec::new();
    Mixer::default().play(&report.sound_writes, report.clock, &mut samples);
    samples
}

/// Saves `picture`, frame `frame`, as a BMP file in `folder`; returns its path.
///
/// # Errors
/// Creating `folder` or writing the file.
pub fn save_picture(frame: u64, picture: &Frame, folder: &Path) -> Result<PathBuf> {
    fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    let path = folder.join(format!("frame-{frame:05}.bmp"));
    fs::write(&path, picture.to_bmp()).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// What a run from reset ended with, and what the runtime saw on the way.
#[derive(Debug)]
pub struct Report {
    pub ssp: u32,
    pub reset_pc: u32,
    pub exit: Exit,
    pub cycles: u64,
    pub frames: u64,
    pub vblanks: u64,
    pub hblanks: u64,
    /// Interrupts taken late, at a native check point (see `Runtime::late_interrupts`).
    pub late: u64,
    /// Check points whose slow path ran the rest of their block as stepped code.
    pub slow_paths: u64,
    /// The status register's system byte at the end.
    pub sr: u16,
    pub misses: Vec<(u32, Miss)>,
    pub accesses: Vec<(Access, u64)>,
    pub unwinds: u64,
    pub tmss_unlocked: bool,
    /// Z80 RAM bytes that are not zero.
    pub z80_bytes: usize,
    /// Where the Z80 is, and its lines: `None` while in reset.
    pub z80_pc: Option<u16>,
    pub z80_bank: u16,
    pub sound: SoundCounts,
    /// Every sound write, when the run kept them ([`Trace::sound`]).
    pub sound_writes: Vec<SoundWrite>,
    /// The master clock the run reached; the sound writes all fall before it.
    pub clock: u64,
    pub vdp: Vdp,
    /// Work RAM at the end.
    pub ram: Box<[u8; RAM_SIZE]>,
    /// The frames asked for, by number, as drawn when they ended.
    pub pictures: Vec<(u64, Frame)>,
}

/// SHA-1s of the VDP's memories and registers, words in big-endian order, for comparing runs.
pub fn vdp_hashes(vdp: &Vdp) -> [(&'static str, Sha1Digest); 4] {
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
    /// What `runtime` holds after a run that ended with `exit`, with `pictures` kept on the
    /// way.
    fn new(runtime: &Runtime, exit: Exit, pictures: Vec<(u64, Frame)>) -> Self {
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
            slow_paths: runtime.slow_paths(),
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
            z80_pc: (!runtime.z80().in_reset()).then(|| runtime.z80().cpu().pc),
            z80_bank: runtime.z80().bank(),
            sound: runtime.sound_counts(),
            sound_writes: Vec::new(),
            clock: runtime.clock(),
            vdp: runtime.hardware::<Vdp>().cloned().unwrap_or_default(),
            ram: Box::new(state.ram),
            pictures,
        }
    }

    /// Prints the report as the command line shows it.
    pub fn print(&self) {
        println!("reset: SSP ${:08X}, PC ${:06X}", self.ssp, self.reset_pc);
        println!(
            "run ended: {} after {} cycles, {} frames",
            self.exit, self.cycles, self.frames
        );
        println!(
            "interrupts taken: {} VBlank (level 6), {} HBlank (level 4); SR ${:04X}",
            self.vblanks, self.hblanks, self.sr
        );
        println!(
            "interrupt delivery: {} late, {} slow paths run as stepped code",
            self.late, self.slow_paths
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
        match self.z80_pc {
            Some(pc) => println!("Z80: PC ${pc:04X}, bank ${:03X}", self.z80_bank),
            None => println!("Z80: in reset"),
        }
        let sound = &self.sound;
        println!(
            "sound writes (68000, Z80): YM2612 {:?} (DAC {:?}, timer runs {:?}), PSG {:?}",
            sound.ym2612, sound.dac, sound.timers, sound.psg
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
        println!("work RAM SHA-1 {}", Sha1Digest::of(&self.ram[..]));
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
/// vertical interrupt; on the reference emulator's console when `reference` is set.
///
/// # Errors
/// The thread failing to start or panicking, or what [`conclude`] returns.
pub fn run_from_reset(
    game: Game,
    rom: Rom,
    frames: u64,
    dumps: &[u64],
    trace: Trace,
    reference: bool,
) -> Result<Report> {
    let dumps = dumps.to_vec();
    let worker = thread::Builder::new()
        .name("68000".into())
        .stack_size(RUN_STACK)
        .spawn(move || {
            let mut runtime = start(rom, game, reference);
            let failure = Failure::default();
            let sound = trace.sound;
            runtime.record_sound(sound);
            let recording = if trace.is_empty() {
                None
            } else {
                trace.hook(&mut runtime, game, Rc::clone(&failure))
            };
            let (exit, pictures) = run_frames(&mut runtime, frames, &dumps);
            let recording = recording.as_ref();
            let mut report = conclude(&runtime, exit, pictures, &failure, recording)?;
            if sound {
                report.sound_writes = runtime.take_sound_writes();
            }
            Ok(report)
        })
        .context("starting the 68000 thread")?;
    worker
        .join()
        .map_err(|_| anyhow!("the 68000 thread panicked"))?
}

/// Runs from reset in a window until it closes, or the run stops at a wait nothing can end,
/// applying the window's pad at every vertical interrupt and doing there what `trace` asks
/// besides (its own pad source is replaced); on the reference emulator's console when
/// `reference` is set. The report covers the run since the last reset.
fn run_in_window(
    game: Game,
    rom: Rom,
    trace: Trace,
    reference: bool,
    controls: Controls,
) -> Result<Report> {
    let (settings, settings_path) = load_settings();
    let config = Config {
        title: format!("AnyGenesis - {}", game.name),
        width: MAX_WIDTH + 2 * game.widescreen,
        scale: WINDOW_SCALE,
        controls,
        settings,
        settings_path,
        reset_refused: trace
            .record
            .is_some()
            .then(|| "the run is recording its input".to_owned()),
    };
    let mut trace = Some(trace);
    anygenesis_frontend::run(
        &config,
        move |pad| {
            let mut runtime = start(rom.clone(), game, reference);
            let failure = Failure::default();
            // Only a run that records nothing is reset, so after the first run the default
            // trace is the one it was given.
            let trace = Trace {
                pad: PadSource::Live(pad),
                ..trace.take().unwrap_or_default()
            };
            let recording = trace.hook(&mut runtime, game, Rc::clone(&failure));
            (runtime, (failure, recording))
        },
        move |runtime, exit, (failure, recording)| {
            let recording = recording.as_ref();
            conclude(runtime, exit, Vec::new(), &failure, recording)
        },
    )?
}

/// The window's controls: from `path`, else from the per-user file if there is one, else
/// the defaults; says which.
///
/// # Errors
/// SDL finding no per-user folder, `path` missing, or a controls file that cannot be read
/// or is refused.
fn load_controls(path: Option<&Path>) -> Result<Controls> {
    let named = path.is_some();
    let path = match path {
        Some(path) => path.to_owned(),
        None => controls::default_path().context("finding the controls file")?,
    };
    let loaded =
        Controls::load(&path).with_context(|| format!("controls file {}", path.display()))?;
    match loaded {
        Some(controls) => {
            println!("controls from {}", path.display());
            Ok(controls)
        }
        None if named => bail!("controls file {} not found", path.display()),
        None => {
            println!("controls: defaults ({} not found)", path.display());
            Ok(Controls::default())
        }
    }
}

/// The window's remembered settings and where to write them back; says which. The file is
/// the window's own, so a missing folder or a file that cannot be used only costs the
/// memory: the run goes on with the defaults.
fn load_settings() -> (Settings, Option<PathBuf>) {
    let path = match settings::default_path() {
        Ok(path) => path,
        Err(error) => {
            println!("settings: defaults, not saved ({error})");
            return (Settings::default(), None);
        }
    };
    let settings = match Settings::load(&path) {
        Ok(Some(settings)) => {
            println!("settings from {}", path.display());
            settings
        }
        Ok(None) => Settings::default(),
        Err(error) => {
            println!("settings: defaults ({}: {error})", path.display());
            Settings::default()
        }
    };
    println!("picture scaling: {}", settings.scaling.name());
    (settings, Some(path))
}

/// The runtime for `game` at reset on the VDP, ending a run at a wait nothing can end; on
/// the reference emulator's console when `reference` is set.
pub fn start(rom: Rom, game: Game, reference: bool) -> Runtime {
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
    native(rom, game, options)
}

/// The runtime for `game` at reset on the VDP with `options`, its stepped code attached and
/// the RAM patches and flags its patch packs carry applied.
pub fn native(rom: Rom, game: Game, mut options: Options) -> Runtime {
    options.ram_patches.extend(game.ram_patches.iter().cloned());
    let mut vdp = Box::<Vdp>::default();
    vdp.set_sprite_limit(game.sprite_limit);
    vdp.set_widescreen(game.widescreen);
    Runtime::new(rom, game.table, vdp, options).with_stepped(
        game.steps,
        game.step_entry,
        game.interrupt,
    )
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
