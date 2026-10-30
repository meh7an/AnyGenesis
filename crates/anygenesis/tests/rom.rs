//! The game exe anygenesis builds from the ROM `ANYGENESIS_ROM` names: any ROM must build
//! into one exe with warnings as errors; Sonic 1 REV00's exe is also run from a folder of
//! its own with no ROM in it. Ignored: needs `ANYGENESIS_ROM` and the player library next to
//! anygenesis, built in the same profile (`cargo build -p anygenesis-player-lib`,
//! `--release` for a release run). The build goes to a cache folder under the system temp
//! folder, not the per-user one, and is made once for all the tests here.

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
    ("VRAM", "bd567c0e7e1284df2bcccd28e2fd803f42453893"),
    ("CRAM", "cfb39dfc175a8a1b73f031801c50a194778752e5"),
    ("VSRAM", "9faab1c6fb9abfc6d5a3c2a3d3149f0f3f7e4f4d"),
    ("registers", "6f90e45454834c6814184810aa3cd4d03cc6e8c4"),
];
const PICTURES: [(u64, &str); 2] = [
    (240, "87e9a8f99cee2471fa47e8d853165035cfbd9d39"),
    (600, "aa2a2bf3b06befd407053e2b43971e8fb630e6d6"),
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
#[ignore = "needs ANYGENESIS_ROM pointing at a ROM image and the player library"]
fn test_any_rom_builds_into_a_lone_game_exe() {
    let exe = game_exe();
    let size = fs::metadata(&exe).expect("the exe has metadata").len();
    println!("game exe        {} ({size} bytes)", exe.display());
    assert!(size > 0);
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

/// The hook block the emitter writes before the instruction at `entry` for the code patch
/// `function`, in translated and stepped code alike.
fn hook_block(function: &str, entry: u32) -> String {
    let a = format!("0x{entry:X}u");
    format!(
        "    {{\n        uint32_t ag_h = {function}(s, {a}) & 0xFFFFFFu;\n        \
         if (ag_h != {a}) {{\n            {{\n                \
         AgFn *next = ag_lookup(s, ag_h);\n                \
         AG_TAIL return next(s, ag_h);\n            }}\n        }}\n    }}\n"
    )
}

/// The `.c` and `.h` files of a build's C folder, by name.
fn c_files(folder: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = fs::read_dir(folder)
        .expect("the C folder reads")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c" || ext == "h"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            (name, fs::read_to_string(&path).expect("the C reads"))
        })
        .collect();
    files.sort();
    files
}

/// `files` with the stepped chunks (`ag_s` and digits) merged into one entry holding their
/// functions one after another: the chunks are cut at a line budget, so a few more lines
/// move the cuts, and with them the declarations each chunk starts with.
fn merge_stepped_chunks(files: Vec<(String, String)>) -> Vec<(String, String)> {
    let is_chunk = |name: &str| {
        name.strip_prefix("ag_s")
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    let mut functions = String::new();
    let mut merged: Vec<(String, String)> = files
        .into_iter()
        .filter_map(|(name, text)| {
            if !is_chunk(&name) {
                return Some((name, text));
            }
            let start = text.find("\nuint32_t ").unwrap_or(text.len());
            functions.push_str(&text[start..]);
            None
        })
        .collect();
    merged.push(("the stepped functions".to_owned(), functions));
    merged
}

/// Runs `exe` headless for 600 frames in `folder`, saving frames 240 and 600, and returns
/// its report and the two pictures.
fn run_600(exe: &Path, folder: &Path) -> (String, Vec<Vec<u8>>) {
    fs::create_dir_all(folder).expect("the run folder is created");
    let output = Command::new(exe)
        .args(["600", "--dump", "240,600"])
        .current_dir(folder)
        .output()
        .expect("the game runs");
    assert!(output.status.success(), "{}", text(&output.stderr));
    let pictures = PICTURES
        .iter()
        .map(|(frame, _)| {
            let path = folder.join("dumps").join(format!("frame-{frame:05}.bmp"));
            fs::read(&path).expect("the picture is saved")
        })
        .collect();
    (text(&output.stdout), pictures)
}

#[test]
#[ignore = "needs ANYGENESIS_ROM set to Sonic 1 REV00 and the player library"]
fn test_sonic1_with_an_empty_hook_at_reset_runs_as_the_plain_exe() {
    // The plain build first: it starts the test folder afresh.
    let plain = game_exe();
    let root = test_root().join("noop");
    fs::create_dir_all(&root).expect("the test folder is created");
    let rom = fs::read(rom_path()).expect("the ROM reads");
    // The reset entry, from the ROM's own vector, so no address is written down here.
    let entry = u32::from_be_bytes([rom[4], rom[5], rom[6], rom[7]]) & 0xFF_FFFF;
    fs::write(
        root.join("noop.c"),
        "uint32_t noop(AgState *s, uint32_t pc)\n{\n    (void)s;\n    return pc;\n}\n",
    )
    .expect("the C is written");
    let pack = root.join("noop.toml");
    let rom_sha1 = Sha1Digest::of(&rom);
    fs::write(
        &pack,
        format!(
            "name = \"No-op\"\nrom = \"{rom_sha1}\"\n\n\
             [[hook]]\naddress = {entry:#x}\nfile = \"noop.c\"\nfunction = \"noop\"\n"
        ),
    )
    .expect("the pack is written");
    let out = root.join("game");
    let built = anygenesis(&[
        &rom_path(),
        Path::new("--patch"),
        &pack,
        Path::new("--out"),
        &out,
    ]);
    println!("{}{}", text(&built.stdout), text(&built.stderr));
    assert!(built.status.success(), "the patched build succeeds");
    let patched = out.join(format!(
        "{} (No-op){}",
        rom_path().file_stem().expect("a stem").to_string_lossy(),
        env::consts::EXE_SUFFIX
    ));

    // The C differs only by the hook blocks, the prototype and the patch files.
    let cache = test_root().join("cache");
    let plain_c = merge_stepped_chunks(c_files(&cache.join(rom_sha1.to_string()).join("c")));
    let patched_folder = fs::read_dir(&cache)
        .expect("the cache reads")
        .map(|entry| entry.expect("an entry").path())
        .find(|path| path.to_string_lossy().contains(&format!("{rom_sha1}-")))
        .expect("the patched build has its own entry");
    let mut patched_c = merge_stepped_chunks(c_files(&patched_folder.join("c")));
    patched_c.retain(|(name, _)| !matches!(name.as_str(), "ag_patch.h" | "ag_patch_000.c"));
    assert_eq!(patched_c.len(), plain_c.len());
    let block = hook_block("noop", entry);
    let prototype = "/* The code patches (ag_patch.h). */\n\
                     uint32_t noop(AgState *s, uint32_t pc);\n\n";
    let mut blocks = 0;
    for ((name, plain), (patched_name, patched)) in plain_c.iter().zip(&patched_c) {
        assert_eq!(name, patched_name);
        blocks += patched.matches(&block).count();
        let stripped = patched.replace(&block, "").replacen(prototype, "", 1);
        assert!(stripped == *plain, "{name} differs by more than the hook");
    }
    assert_eq!(
        blocks, 2,
        "one hook in the translated code, one in the stepped code"
    );

    // And the runs are the same.
    let (plain_report, plain_pictures) = run_600(&plain, &root.join("plain-run"));
    let (patched_report, patched_pictures) = run_600(&patched, &root.join("patched-run"));
    println!("{patched_report}");
    assert!(
        patched_report.contains(" cycles, 600 frames\n"),
        "{patched_report}"
    );
    assert_eq!(patched_report, plain_report);
    assert!(patched_pictures == plain_pictures, "the pictures differ");
    for ((frame, hash), picture) in PICTURES.iter().zip(&patched_pictures) {
        assert_eq!(Sha1Digest::of(picture).to_string(), *hash, "frame {frame}");
    }
}
