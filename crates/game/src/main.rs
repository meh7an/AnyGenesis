//! The in-process game build: the game `ANYGENESIS_ROM` named when this crate was built,
//! translated, linked into the binary and run by the player, for the game's tests and for
//! runs straight from cargo. anygenesis builds the same program, table and image included,
//! as a single game exe; the command line is the player's (see anygenesis-player).

mod game;

use anygenesis_player::Game;
use anyhow::{Context, Result};

fn main() -> Result<()> {
    let table = game::table()
        .context("built without a game: set ANYGENESIS_ROM to the ROM, then rebuild")?;
    anygenesis_player::main(Game::from_table(table)?)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        ops::Range,
        path::Path,
        rc::Rc,
        thread,
    };

    use anygenesis_abi::ram_index;
    use anygenesis_framediff::{Folder, Input, Masks, Recorder};
    use anygenesis_frontend::Pad;
    use anygenesis_player::{
        DEFAULT_FRAMES, Failure, PadSource, Report, Trace, conclude, native, run_from_reset,
        save_picture, start, vdp_hashes,
    };
    use anygenesis_rom::{Rom, Sha1Digest};
    use anygenesis_runtime::{Buttons, Exit, Options, RUN_STACK, Vdp, vdp::CRAM_MASK};

    use super::*;

    /// Sega Retro, "Sonic the Hedgehog (Mega Drive)/Technical information", Revision 00.
    const SONIC1_REV00_SHA1: &str = "6ddb7de1e17e7f6cdb88927bd906352030daa194";
    /// The game mode byte in work RAM, and its values on the title screen and in a level.
    const GAME_MODE: u32 = 0xFF_F600;
    const TITLE_MODE: u8 = 0x04;
    const LEVEL_MODE: u8 = 0x0C;
    /// The copy of CRAM in work RAM that the vertical interrupt copies to CRAM.
    const PALETTE_COPY: u32 = 0xFF_FB00;
    /// The colours the title screen rotates by a step every few frames, in the main loop.
    const CYCLED_COLOURS: Range<usize> = 40..44;
    /// The ROM table of 19 `0x8RVV` words that sets VDP registers 0-18 at boot.
    const VDP_SETUP: u32 = 0x00_129E;

    /// The VDP's memories and registers after 600 frames, as Mehran signed them off
    /// (M3.3, VRAM again at v0.2 M1.1, VRAM and CRAM at v0.2 M3.2); they change whenever
    /// DMA, timing or the translated code's cycle counts do.
    const HASHES_AT_600: [(&str, &str); 4] = [
        ("VRAM", "bd567c0e7e1284df2bcccd28e2fd803f42453893"),
        ("CRAM", "cfb39dfc175a8a1b73f031801c50a194778752e5"),
        ("VSRAM", "9faab1c6fb9abfc6d5a3c2a3d3149f0f3f7e4f4d"),
        ("registers", "6f90e45454834c6814184810aa3cd4d03cc6e8c4"),
    ];
    /// A frame showing the SEGA screen.
    const SEGA_FRAME: u64 = 240;
    /// SHA-1s of the BMP files of the SEGA frame and of frame 600 (the title screen), as
    /// Mehran signed the pictures off (M3.4, frame 600 again at v0.2 M1.1 and M3.2); they
    /// change whenever the hashes above or the renderer do.
    const PICTURES: [(u64, &str); 2] = [
        (SEGA_FRAME, "87e9a8f99cee2471fa47e8d853165035cfbd9d39"),
        (DEFAULT_FRAMES, "aa2a2bf3b06befd407053e2b43971e8fb630e6d6"),
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

    /// The translated game, which must be Sonic 1 REV00, and the ROM image it carries.
    fn sonic1() -> (Game, Rom) {
        let table = game::table().expect("the game crate was built with ANYGENESIS_ROM set");
        let game = Game::from_table(table).expect("the game table checks out");
        assert_eq!(
            game.sha1.to_string(),
            SONIC1_REV00_SHA1,
            "these tests need a build of Sonic 1 REV00"
        );
        let rom = game.rom().expect("the embedded image loads");
        (game, rom)
    }

    /// The game mode byte at the end of the run `report` describes.
    fn game_mode(report: &Report) -> u8 {
        report.ram[ram_index(GAME_MODE)]
    }

    /// Sonic 1 from reset for ten seconds of frames on the VDP, with interrupts and DMA,
    /// saving the SEGA and title screen pictures to `dumps/` at the workspace root.
    #[test]
    #[ignore = "needs a build with ANYGENESIS_ROM set to Sonic 1 REV00"]
    fn test_sonic1_runs_600_frames_without_misses() {
        let (game, rom) = sonic1();
        let registers = title_registers(&rom, VDP_SETUP);
        let dumps = PICTURES.map(|(frame, _)| frame);
        // An empty input still sets the hook at every vertical interrupt, which must change
        // nothing below.
        let trace = Trace {
            pad: PadSource::Replay(Input::default()),
            ..Trace::default()
        };
        let report = run_from_reset(game, rom, DEFAULT_FRAMES, &dumps, trace, false)
            .expect("the run finishes");
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dumps");
        for (frame, picture) in &report.pictures {
            save_picture(*frame, picture, &folder).expect("the picture is saved");
        }
        report.print();
        assert!(
            matches!(report.exit, Exit::Frames { .. }),
            "{}",
            report.exit
        );
        assert_eq!(report.frames, DEFAULT_FRAMES);
        assert_eq!(report.late, 0);
        assert!(report.misses.is_empty(), "{:?}", report.misses);
        assert!(report.accesses.is_empty(), "{:?}", report.accesses);
        assert_eq!(report.unwinds, 0);
        assert!(report.vblanks > 0);
        assert!(report.tmss_unlocked);
        assert!(report.z80_bytes > 0);
        assert_eq!(game_mode(&report), TITLE_MODE);

        // The title screen has faded in, so the last VBlank's DMA left CRAM equal to the
        // palette buffer, but for the cycled colours: the main loop may have rotated them
        // since.
        let palette: Vec<u16> = (0..64)
            .map(|index| {
                let at = ram_index(PALETTE_COPY) + 2 * index;
                u16::from_be_bytes([report.ram[at], report.ram[at + 1]]) & CRAM_MASK
            })
            .collect();
        let cram = report.vdp.cram();
        for (index, (&shown, &buffered)) in cram.iter().zip(&palette).enumerate() {
            if !CYCLED_COLOURS.contains(&index) {
                assert_eq!(shown, buffered, "colour {index}");
            }
        }
        let cycled = &palette[CYCLED_COLOURS];
        let rotated = (0..cycled.len()).any(|step| {
            cycled
                .iter()
                .cycle()
                .skip(step)
                .zip(&cram[CYCLED_COLOURS])
                .all(|(buffered, shown)| buffered == shown)
        });
        assert!(
            rotated,
            "{:?} not a rotation of {cycled:?}",
            &cram[CYCLED_COLOURS]
        );
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
    /// with room for the native stack. Also returns the late interrupts and slow paths.
    fn outcome(game: Game, rom: Rom, frames: u64, slice: i32) -> (Outcome, u64, u64) {
        let worker = thread::Builder::new()
            .name("68000".into())
            .stack_size(RUN_STACK)
            .spawn(move || {
                let options = Options {
                    slice,
                    ..Options::default()
                };
                let mut runtime = native(rom, game, options);
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
                (outcome, runtime.late_interrupts(), runtime.slow_paths())
            })
            .expect("the 68000 thread starts");
        worker.join().expect("the run finishes")
    }

    /// Stepped code against translated blocks: with one-cycle budgets every budget ends
    /// inside its block, so every block runs as stepped code, and the machine must end as it
    /// does with native blocks. Only an interrupt native code took late could tell them
    /// apart.
    #[test]
    #[ignore = "needs a build with ANYGENESIS_ROM set to Sonic 1 REV00"]
    fn test_sonic1_stepped_blocks_end_where_native_ones_do() {
        let (game, rom) = sonic1();
        let slice = Options::default().slice;
        let (native, late, slow_paths) = outcome(game, rom.clone(), DEFAULT_FRAMES, slice);
        let (stepped, stepped_late, every) = outcome(game, rom, DEFAULT_FRAMES, 1);
        println!(
            "native: {late} late, {slow_paths} slow paths; one-cycle budgets: {stepped_late} \
             late, {every} slow paths"
        );
        assert_eq!((late, stepped_late), (0, 0));
        assert_eq!(native, stepped);
    }

    /// A game binary holds no interpreter (Architecture Decision 6): the game crate leaves
    /// the runtime's `interpreter` feature out. Building the whole workspace turns it on for
    /// every member, so this holds only for the crate built alone, as `cargo test -p
    /// anygenesis-game` builds it.
    #[test]
    #[ignore = "holds only for the game crate built alone (cargo test -p anygenesis-game)"]
    fn test_game_has_no_interpreter() {
        assert!(!anygenesis_runtime::has_interpreter());
    }

    /// Start held for ten vertical interrupts on the idle title screen (stop 298 ends frame
    /// 600), in the frame diff's input format.
    const START_PRESS: &str = "330 S\n340 -\n";
    /// Frames the Start run lasts.
    const START_FRAMES: u64 = 900;

    /// Pressing Start on the title screen leaves it for the first level.
    #[test]
    #[ignore = "needs a build with ANYGENESIS_ROM set to Sonic 1 REV00"]
    fn test_sonic1_title_screen_accepts_start() {
        let (game, rom) = sonic1();
        let trace = Trace {
            pad: PadSource::Replay(Input::parse(START_PRESS).expect("the input is valid")),
            ..Trace::default()
        };
        let report =
            run_from_reset(game, rom, START_FRAMES, &[], trace, false).expect("the run finishes");
        report.print();
        assert!(
            matches!(report.exit, Exit::Frames { .. }),
            "{}",
            report.exit
        );
        assert_eq!(report.frames, START_FRAMES);
        assert_eq!(report.late, 0);
        assert_eq!(report.unwinds, 0);
        assert_eq!(game_mode(&report), LEVEL_MODE);
    }

    /// Frames the recorded play lasts.
    const PLAY_FRAMES: u64 = 1_500;

    /// A scripted player, by frame: Start on the idle title screen, then running right in
    /// the level with a jump held for 45 frames out of every 90.
    fn scripted(frame: u64) -> Buttons {
        match frame {
            660..700 => Buttons::START,
            900.. if frame / 45 % 2 == 1 => Buttons(Buttons::RIGHT.0 | Buttons::C.0),
            900.. => Buttons::RIGHT,
            _ => Buttons::NONE,
        }
    }

    /// Plays `frames` frames from reset one at a time as the window does, on the reference
    /// console, holding [`scripted`]'s buttons on the pad the hook applies, recording what
    /// it applied to `path` and saving every stop in `folder`.
    fn play(game: Game, rom: Rom, frames: u64, path: &Path, folder: &Path) -> Report {
        let record = Recorder::new(File::create(path).expect("the recording is created"));
        let folder = folder.to_owned();
        let worker = thread::Builder::new()
            .name("68000".into())
            .stack_size(RUN_STACK)
            .spawn(move || {
                let mut runtime = start(rom, game, true);
                let pad = Pad::default();
                let trace = Trace {
                    folder: Some(folder),
                    pad: PadSource::Live(pad.clone()),
                    record: Some(record),
                    sound: false,
                };
                let failure = Failure::default();
                let recording = trace.hook(&mut runtime, game, Rc::clone(&failure));
                let exit = loop {
                    pad.set(scripted(runtime.frames() + 1));
                    let exit = runtime.run_frames(1);
                    if runtime.frames() == frames || !matches!(exit, Exit::Frames { .. }) {
                        break exit;
                    }
                };
                let recording = recording.as_ref();
                conclude(&runtime, exit, Vec::new(), &failure, recording)
            })
            .expect("the 68000 thread starts");
        worker
            .join()
            .expect("the run finishes")
            .expect("the play is recorded")
    }

    /// What two runs of one machine must agree on at the end.
    fn summary(report: &Report) -> Vec<String> {
        let mut summary = vec![
            report.exit.to_string(),
            format!("{} cycles, {} frames", report.cycles, report.frames),
            format!("{} VBlanks, SR ${:04X}", report.vblanks, report.sr),
            Sha1Digest::of(&report.ram[..]).to_string(),
        ];
        summary.extend(vdp_hashes(&report.vdp).map(|(_, hash)| hash.to_string()));
        summary
    }

    /// A play recorded frame by frame from a live pad replays from the recording, in one
    /// headless run as the command line does, to the same machine at every stop.
    #[test]
    #[ignore = "needs a build with ANYGENESIS_ROM set to Sonic 1 REV00"]
    fn test_sonic1_recorded_play_replays_to_the_same_machine() {
        let (game, rom) = sonic1();
        let folder =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dumps/sonic1-recording-test");
        // Stops of an earlier, longer run would outlast this one's.
        let _ = fs::remove_dir_all(&folder);
        let (live, replay) = (folder.join("played"), folder.join("replayed"));
        for stops in [&live, &replay] {
            fs::create_dir_all(stops).expect("the state folder is created");
        }
        let path = folder.join("input.txt");
        let played = play(game, rom.clone(), PLAY_FRAMES, &path, &live);
        played.print();
        let input = Input::load(&path).expect("the recording parses");
        let text = fs::read_to_string(&path).expect("the recording reads");
        let changes = text.lines().filter(|line| !line.starts_with('#')).count();
        let end = format!("# end: stop {}, frame {PLAY_FRAMES}", played.vblanks);
        assert!(changes > 10, "{text}");
        assert_eq!(text.lines().last(), Some(end.as_str()));

        let trace = Trace {
            folder: Some(replay.clone()),
            pad: PadSource::Replay(input),
            record: None,
            sound: false,
        };
        let replayed =
            run_from_reset(game, rom, PLAY_FRAMES, &[], trace, true).expect("the replay finishes");
        replayed.print();
        assert_eq!((played.late, replayed.late), (0, 0));
        assert_eq!(summary(&replayed), summary(&played));
        let stops =
            anygenesis_framediff::diff(&mut Folder(replay), &mut Folder(live), &Masks::default())
                .expect("the stops read back");
        assert!(stops.passed(), "{stops}");
        assert_eq!(game_mode(&replayed), LEVEL_MODE);
    }
}
