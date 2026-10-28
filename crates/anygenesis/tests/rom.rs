//! The game exe anygenesis builds from Sonic 1 REV00, run from a folder of its own with no
//! ROM in it. Ignored: needs `ANYGENESIS_ROM` and the player library next to anygenesis,
//! built in the same profile (`cargo build -p anygenesis-player-lib`, `--release` for a
//! release run). The build goes to a cache folder under the system temp folder, not the
//! per-user one, and is made once for all the tests here.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

use anygenesis_rom::Sha1Digest;

/// Sega Retro, "Sonic the Hedgehog (Mega Drive)/Technical information", Revision 00.
const SONIC1_REV00_SHA1: &str = "6ddb7de1e17e7f6cdb88927bd906352030daa194";
/// The signed-off VDP hashes after 600 frames and BMP hashes of frames 240 and 600, as the
/// game crate's tests hold them (re-signing changes both places).
const HASHES_AT_600: [(&str, &str); 4] = [
    ("VRAM", "af90156571d5d2313cb701d0568ffa7552674efa"),
    ("CRAM", "2ab1aba063597f9054b94bbf379bda288be40369"),
    ("VSRAM", "9faab1c6fb9abfc6d5a3c2a3d3149f0f3f7e4f4d"),
    ("registers", "6f90e45454834c6814184810aa3cd4d03cc6e8c4"),
];
const PICTURES: [(u64, &str); 2] = [
    (240, "87e9a8f99cee2471fa47e8d853165035cfbd9d39"),
    (600, "58274d550398349f85b1745ff30870605aa13f11"),
];
/// The game mode byte in work RAM (offset in the RAM dump) and its value in a level.
const GAME_MODE: usize = 0xF600;
const LEVEL_MODE: u8 = 0x0C;
/// Start held for ten vertical interrupts on the idle title screen.
const START_PRESS: &str = "330 S\n340 -\n";

/// The folder the tests build and run in.
fn test_root() -> PathBuf {
    env::temp_dir().join("anygenesis-rom-test")
}

fn rom_path() -> PathBuf {
    PathBuf::from(env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is set"))
}

/// Runs anygenesis with `arguments` on the test's own cache.
fn anygenesis(arguments: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anygenesis"))
        .args(arguments)
        .env("ANYGENESIS_CACHE", test_root().join("cache"))
        .output()
        .expect("anygenesis runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Builds the game exe once, from a fresh cache, then again from the cache, and returns
/// the folder it is in, which holds nothing else.
fn game_folder() -> &'static Path {
    static FOLDER: OnceLock<PathBuf> = OnceLock::new();
    FOLDER.get_or_init(|| {
        let root = test_root();
        let _ = fs::remove_dir_all(&root);
        let out = root.join("game");
        let rom = rom_path();
        let built = anygenesis(&[&rom, Path::new("--out"), &out]);
        let stdout = text(&built.stdout);
        println!("{stdout}{}", text(&built.stderr));
        assert!(built.status.success(), "the build succeeds");
        assert!(stdout.contains("building in"), "{stdout}");
        let again = anygenesis(&[&rom, Path::new("--out"), &out]);
        assert!(again.status.success());
        assert!(
            text(&again.stdout).contains("using the cached build"),
            "{}",
            text(&again.stdout)
        );
        out
    })
}

/// The game exe, the only file in its folder.
fn game_exe() -> PathBuf {
    let entries: Vec<PathBuf> = fs::read_dir(game_folder())
        .expect("the game folder reads")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(entries.len(), 1, "only the exe is there: {entries:?}");
    entries[0].clone()
}

/// Runs the game exe in its own folder with `arguments`.
fn run_game(arguments: &[&str]) -> Output {
    Command::new(game_exe())
        .args(arguments)
        .current_dir(game_folder())
        .output()
        .expect("the game runs")
}

#[test]
#[ignore = "needs ANYGENESIS_ROM set to Sonic 1 REV00 and the player library"]
fn test_sonic1_game_exe_runs_600_frames_alone_as_signed_off() {
    let output = run_game(&["600", "--dump", "240,600"]);
    let stdout = text(&output.stdout);
    println!("{stdout}{}", text(&output.stderr));
    assert!(output.status.success());
    assert!(stdout.contains(&format!(
        "game SONIC THE HEDGEHOG, ROM SHA-1 {SONIC1_REV00_SHA1}"
    )));
    assert!(stdout.contains(" cycles, 600 frames\n"), "{stdout}");
    assert!(stdout.contains("interrupt delivery: 0 late,"), "{stdout}");
    assert!(stdout.contains("logged accesses: 0\n"), "{stdout}");
    for (name, hash) in HASHES_AT_600 {
        assert!(
            stdout.contains(&format!("VDP {name} SHA-1 {hash}\n")),
            "{name}"
        );
    }
    for (frame, hash) in PICTURES {
        let path = game_folder()
            .join("dumps")
            .join(format!("frame-{frame:05}.bmp"));
        let bytes = fs::read(&path).expect("the picture is saved");
        assert_eq!(Sha1Digest::of(&bytes).to_string(), hash, "frame {frame}");
    }
}

#[test]
#[ignore = "needs ANYGENESIS_ROM set to Sonic 1 REV00 and the player library"]
fn test_sonic1_game_exe_refuses_a_rom_file() {
    let rom = rom_path();
    let output = run_game(&[rom.to_str().expect("a UTF-8 ROM path")]);
    let stderr = text(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("takes no ROM file"), "{stderr}");
}

#[test]
#[ignore = "needs ANYGENESIS_ROM set to Sonic 1 REV00 and the player library"]
fn test_sonic1_game_exe_replays_input_into_the_level() {
    // The build starts the test folder afresh, so it comes before anything is written there.
    game_folder();
    let root = test_root();
    let (input, states) = (root.join("start.txt"), root.join("start-states"));
    fs::write(&input, START_PRESS).expect("the input is written");
    let arguments = [
        "900",
        "--input",
        input.to_str().expect("UTF-8"),
        "--state",
        states.to_str().expect("UTF-8"),
    ];
    let output = run_game(&arguments);
    let stdout = text(&output.stdout);
    println!("{stdout}{}", text(&output.stderr));
    assert!(output.status.success());
    assert!(stdout.contains("interrupt delivery: 0 late,"), "{stdout}");
    let last = fs::read_dir(&states)
        .expect("the states read")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.to_string_lossy().ends_with("-ram.bin"))
        .max()
        .expect("stops were saved");
    let ram = fs::read(&last).expect("the last stop's RAM reads");
    assert_eq!(ram[GAME_MODE], LEVEL_MODE, "{}", last.display());
}
