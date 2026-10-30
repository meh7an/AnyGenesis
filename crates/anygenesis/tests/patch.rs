//! A synthetic ROM patched by a pack that holds every ROM-level patch kind (an IPS file,
//! inline bytes, a Game Genie code) builds like any other ROM, with no hints: its analysis
//! finds the code only the patches make reachable. A pack's code patches (M6.2), a C hook, a
//! C replacement and RAM patches at reset and at VBlank, run in the built game, and so do
//! the `widescreen` flag and a hook's `ag_set_view` (M6.3). The
//! whole-program builds are ignored: they need the player library next to anygenesis, built
//! in the same profile (`cargo build -p anygenesis-player-lib`).

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anygenesis_patch::{Patch, PatchError};
use anygenesis_recomp::build;
use anygenesis_rom::{Rom, Sha1Digest};

const SIZE: usize = 0x400;
/// Where every vector points: `bra.s *`, until the pack makes it `jmp $300`.
const RESET: usize = 0x200;
/// Code nothing reaches before patching: `nop` (inline bytes), then `bra.s $300` (the Game
/// Genie code).
const PATCHED: u32 = 0x300;
/// The Game Genie code for the word 0x60FC (`bra.s $300`) at 0x302.
const GENIE: &str = "9TBT-AA2C";

/// A well-formed image: every vector but the stack pointer at `RESET`, where `code` is, a
/// made-up header and a correct checksum.
fn synthetic_rom(code: &[u16]) -> Vec<u8> {
    let mut rom = vec![0; SIZE];
    for vector in rom[..0x100].chunks_mut(4) {
        vector.copy_from_slice(&(RESET as u32).to_be_bytes());
    }
    rom[..4].copy_from_slice(&0x00FF_FE00u32.to_be_bytes());
    rom[0x100..0x200].fill(b' ');
    for (offset, text) in [
        (0x100, "SEGA GENESIS"),
        (0x110, "(C)TEST 2026.OCT"),
        (0x120, "PATCH TEST"),
        (0x150, "PATCH TEST"),
        (0x180, "GM 00000000-00"),
        (0x1F0, "JUE"),
    ] {
        rom[offset..offset + text.len()].copy_from_slice(text.as_bytes());
    }
    for (offset, value) in [(0x1A0, 0), (0x1A4, SIZE as u32 - 1), (0x1A8, 0x00FF_0000)] {
        rom[offset..offset + 4].copy_from_slice(&u32::to_be_bytes(value));
    }
    rom[0x1AC..0x1B0].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
    for (index, word) in code.iter().enumerate() {
        let at = RESET + 2 * index;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    let rom = Rom::from_bytes(rom).expect("the image loads");
    assert!(
        !rom.has_valid_checksum(),
        "the checksum is still to be written"
    );
    rom.with_fixed_checksum().bytes().to_vec()
}

/// A folder of the test's own under the system temp folder, emptied first.
fn folder(name: &str) -> PathBuf {
    let folder = env::temp_dir().join(format!("anygenesis-patch-test-{name}"));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).expect("the test folder is created");
    folder
}

/// Writes the ROM, an IPS file and the pack for it into `folder`, `result` naming the
/// patched image's SHA-1 when given, and returns the ROM's and the pack's paths.
fn write_pack(folder: &Path, result: Option<Sha1Digest>) -> (PathBuf, PathBuf) {
    // bra.s *
    let rom = synthetic_rom(&[0x60FE]);
    let rom_path = folder.join("synthetic.bin");
    fs::write(&rom_path, &rom).expect("the ROM is written");
    let ips = [
        &b"PATCH"[..],
        &[0x00, 0x02, 0x00, 0x00, 0x06],
        &[0x4E, 0xF9, 0x00, 0x00, 0x03, 0x00],
        b"EOF",
    ]
    .concat();
    fs::write(folder.join("jump.ips"), ips).expect("the IPS file is written");
    let result = result.map_or(String::new(), |sha1| format!("result = \"{sha1}\"\n"));
    let pack = format!(
        "name = \"Test pack\"\nrom = \"{}\"\n{result}\n\
         [[patch]]\nfile = \"jump.ips\"\n\n\
         [[patch]]\noffset = {PATCHED:#x}\nbytes = \"4E71\"\n\n\
         [[patch]]\ngenie = [\"{GENIE}\"]\n",
        Sha1Digest::of(&rom)
    );
    let pack_path = folder.join("pack.toml");
    fs::write(&pack_path, pack).expect("the pack is written");
    (rom_path, pack_path)
}

fn patched(rom_path: &Path, pack_path: &Path) -> Result<Rom, PatchError> {
    let rom = build::load_rom(rom_path).expect("the ROM loads");
    anygenesis_patch::apply(&rom, &[Patch::load(pack_path).expect("the pack loads")])
}

fn reaches(rom: &Rom, address: u32) -> bool {
    let analysis = build::translate(rom, build::game_liveness(), &build::game_options()).analysis;
    analysis
        .instructions
        .keys()
        .any(|&start| u32::from(start) == address)
}

#[test]
fn test_patched_synthetic_rom_translates_the_code_its_patches_reach() {
    let folder = folder("translate");
    let (rom_path, pack_path) = write_pack(&folder, None);
    let rom = build::load_rom(&rom_path).expect("the ROM loads");
    let patched = patched(&rom_path, &pack_path).expect("the pack applies");
    let at = |offset: u32| &patched.bytes()[offset as usize..offset as usize + 2];
    assert_eq!(at(PATCHED), [0x4E, 0x71]);
    assert_eq!(at(PATCHED + 2), [0x60, 0xFC]);
    assert!(patched.has_valid_checksum());
    assert_ne!(patched.header().checksum, rom.header().checksum);
    assert_ne!(patched.sha1(), rom.sha1());
    assert!(!reaches(&rom, PATCHED));
    assert!(reaches(&patched, PATCHED) && reaches(&patched, PATCHED + 2));
    fs::remove_dir_all(&folder).expect("the test folder is removed");
}

#[test]
fn test_pack_result_checked_when_the_pack_is_alone() {
    let folder = folder("result");
    let (rom_path, pack_path) = write_pack(&folder, None);
    let sha1 = patched(&rom_path, &pack_path).expect("applies").sha1();
    let (rom_path, pack_path) = write_pack(&folder, Some(sha1));
    assert_eq!(
        patched(&rom_path, &pack_path).expect("applies").sha1(),
        sha1
    );
    let (rom_path, pack_path) = write_pack(&folder, Some(Sha1Digest::of(b"other")));
    let error = patched(&rom_path, &pack_path).expect_err("the result differs");
    assert!(matches!(error, PatchError::WrongResult { .. }), "{error}");
    fs::remove_dir_all(&folder).expect("the test folder is removed");
}

#[test]
#[ignore = "needs the player library next to anygenesis"]
fn test_patched_synthetic_rom_builds_into_a_game_exe_of_its_own() {
    let folder = folder("build");
    let (rom_path, pack_path) = write_pack(&folder, None);
    let sha1 = patched(&rom_path, &pack_path).expect("applies").sha1();
    let out = folder.join("out");
    let built = Command::new(env!("CARGO_BIN_EXE_anygenesis"))
        .arg(&rom_path)
        .args([Path::new("--patch"), &pack_path, Path::new("--out"), &out])
        .env("ANYGENESIS_CACHE", folder.join("cache"))
        .output()
        .expect("anygenesis runs");
    let stdout = String::from_utf8_lossy(&built.stdout);
    println!("{stdout}{}", String::from_utf8_lossy(&built.stderr));
    assert!(built.status.success(), "the build succeeds");
    assert!(stdout.contains(&format!("patched with Test pack: SHA-1 {sha1}")));
    assert!(
        folder
            .join("cache")
            .join(sha1.to_string())
            .join("key")
            .is_file()
    );
    let exe = out.join(format!("synthetic (Test pack){}", env::consts::EXE_SUFFIX));
    assert!(exe.is_file(), "{} is written", exe.display());
    fs::remove_dir_all(&folder).expect("the test folder is removed");
}

/// The program the code patches act on: `move.b #$11,($FF0020).l` at reset, then a loop of
/// `clr.b ($FF0040).l`. The replacement at reset stores 0x22 at $FF0021 instead and goes on
/// at the loop, the hook at the loop stores 0xAB at $FF0010 every time round, and the RAM
/// patches store 0x33 at $FF0030 at reset and 0x44 at $FF0040 at every VBlank point.
const CODE_PROGRAM: [u16; 8] = [
    0x13FC, 0x0011, 0x00FF, 0x0020, 0x4239, 0x00FF, 0x0040, 0x60F8,
];
const LOOP: usize = RESET + 8;
const HOOKS: &str = "uint32_t mark_loop(AgState *s, uint32_t pc)\n{\n    \
                     ag_ram_write8(s, 0xFF0010u, 0xABu);\n    return pc;\n}\n\n\
                     uint32_t skip_store(AgState *s, uint32_t pc)\n{\n    \
                     ag_ram_write8(s, 0xFF0021u, 0x22u);\n    return pc + 8u;\n}\n";

/// Writes the code program's ROM, `hooks.c` and a pack of code patches into `folder`, the
/// hook at `hook`, and returns the ROM's and the pack's paths.
fn write_code_pack(folder: &Path, hook: usize) -> (PathBuf, PathBuf) {
    let rom = synthetic_rom(&CODE_PROGRAM);
    let rom_path = folder.join("synthetic.bin");
    fs::write(&rom_path, &rom).expect("the ROM is written");
    fs::write(folder.join("hooks.c"), HOOKS).expect("the C is written");
    let pack = format!(
        "name = \"Code pack\"\nrom = \"{}\"\n\n\
         [[hook]]\naddress = {hook:#x}\nfile = \"hooks.c\"\nfunction = \"mark_loop\"\n\n\
         [[replace]]\naddress = {RESET:#x}\nfile = \"hooks.c\"\nfunction = \"skip_store\"\n\
         cycles = 20\n\n\
         [[ram]]\naddress = 0xFF0030\nbytes = \"33\"\nwhen = \"reset\"\n\n\
         [[ram]]\naddress = 0xFF0040\nbytes = \"44\"\nwhen = \"vblank\"\n\n\
         [flags]\nsprite_limit = false\n",
        Sha1Digest::of(&rom)
    );
    let pack_path = folder.join("code.toml");
    fs::write(&pack_path, pack).expect("the pack is written");
    (rom_path, pack_path)
}

/// Builds the ROM at `rom_path` with the pack at `pack_path` into `out` on the cache in
/// `folder`, returning what anygenesis printed and whether it succeeded.
fn build_with(folder: &Path, rom_path: &Path, pack_path: &Path, out: &Path) -> (String, bool) {
    let built = Command::new(env!("CARGO_BIN_EXE_anygenesis"))
        .arg(rom_path)
        .args([Path::new("--patch"), pack_path, Path::new("--out"), out])
        .env("ANYGENESIS_CACHE", folder.join("cache"))
        .output()
        .expect("anygenesis runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    println!("{text}");
    (text, built.status.success())
}

#[test]
#[ignore = "needs the player library next to anygenesis"]
fn test_code_patches_run_in_the_built_game() {
    let folder = folder("code");
    let (rom_path, pack_path) = write_code_pack(&folder, LOOP);
    let out = folder.join("out");
    let (text, built) = build_with(&folder, &rom_path, &pack_path, &out);
    assert!(built, "the build succeeds");
    assert!(
        text.contains("code patches: 2 in C, 2 RAM, 1 flags"),
        "{text}"
    );
    let pack = Patch::load(&pack_path).expect("the pack loads");
    let hash = anygenesis_patch::CodePatches::of(&[pack])
        .expect("no clash")
        .hash();
    let sha1 = Sha1Digest::of(&fs::read(&rom_path).expect("the ROM reads"));
    let entry = folder.join("cache").join(format!("{sha1}-{hash}"));
    let key = fs::read_to_string(entry.join("key")).expect("the entry is finished");
    assert!(key.ends_with(&format!("patches {hash}\n")), "{key}");

    let exe = out.join(format!("synthetic (Code pack){}", env::consts::EXE_SUFFIX));
    let run = Command::new(&exe)
        .arg("3")
        .current_dir(&out)
        .output()
        .expect("the game runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    println!("{stdout}{}", String::from_utf8_lossy(&run.stderr));
    assert!(run.status.success());
    assert!(stdout.contains(" cycles, 3 frames\n"), "{stdout}");
    // The run ends at the third VBlank point, right after the VBlank patch.
    let mut ram = vec![0; 0x1_0000];
    for (offset, byte) in [(0x10, 0xAB), (0x21, 0x22), (0x30, 0x33), (0x40, 0x44)] {
        ram[offset] = byte;
    }
    let expected = Sha1Digest::of(&ram);
    assert!(
        stdout.contains(&format!("work RAM SHA-1 {expected}\n")),
        "{stdout}"
    );

    // A hook where no code is refuses the build.
    let (rom_path, pack_path) = write_code_pack(&folder, 0x300);
    let (text, built) = build_with(&folder, &rom_path, &pack_path, &out);
    assert!(!built);
    assert!(text.contains("no code is at 0x000300"), "{text}");
    fs::remove_dir_all(&folder).expect("the test folder is removed");
}

/// The widescreen program (M6.3): TMSS is unlocked with `move.l #'SEGA',($A14000).l`, CRAM 0,
/// the backdrop, set white with `move.l #$C0000000,($C00004).l` and `move.w
/// #$0EEE,($C00000).l`, then `bra.s *`, where a hook shows 8 of the 16 pixels of the left
/// margin and none of the right one.
const WIDE_PROGRAM: [u16; 14] = [
    0x23FC, 0x5345, 0x4741, 0x00A1, 0x4000, 0x23FC, 0xC000, 0x0000, 0x00C0, 0x0004, 0x33FC, 0x0EEE,
    0x00C0, 0x0000,
];
const WIDE_LOOP: usize = RESET + 2 * WIDE_PROGRAM.len();
const WIDE_MARGIN: usize = 16;
const VIEW_HOOK: &str = "uint32_t narrow_view(AgState *s, uint32_t pc)\n{\n    \
                         ag_set_view(s, 8u, 0u);\n    return pc;\n}\n";

#[test]
#[ignore = "needs the player library next to anygenesis"]
fn test_widescreen_flag_and_view_hook_shape_the_built_games_picture() {
    let folder = folder("widescreen");
    let mut code = WIDE_PROGRAM.to_vec();
    code.push(0x60FE);
    let rom = synthetic_rom(&code);
    let rom_path = folder.join("synthetic.bin");
    fs::write(&rom_path, &rom).expect("the ROM is written");
    fs::write(folder.join("view.c"), VIEW_HOOK).expect("the C is written");
    let pack = format!(
        "name = \"Wide pack\"\nrom = \"{}\"\n\n\
         [[hook]]\naddress = {WIDE_LOOP:#x}\nfile = \"view.c\"\nfunction = \"narrow_view\"\n\n\
         [flags]\nwidescreen = {WIDE_MARGIN}\n",
        Sha1Digest::of(&rom)
    );
    let pack_path = folder.join("wide.toml");
    fs::write(&pack_path, pack).expect("the pack is written");
    let out = folder.join("out");
    let (text, built) = build_with(&folder, &rom_path, &pack_path, &out);
    assert!(built, "the build succeeds");
    assert!(
        text.contains("code patches: 1 in C, 0 RAM, 1 flags"),
        "{text}"
    );

    let exe = out.join(format!("synthetic (Wide pack){}", env::consts::EXE_SUFFIX));
    let run = Command::new(&exe)
        .args(["3", "--dump", "3"])
        .current_dir(&out)
        .output()
        .expect("the game runs");
    println!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.status.success());
    let bmp = fs::read(out.join("dumps").join("frame-00003.bmp")).expect("the picture is saved");
    // The program leaves every register at 0: H32, 256 pixels between the margins.
    let width = 256 + 2 * WIDE_MARGIN;
    let width_field = u32::from_le_bytes(bmp[18..22].try_into().expect("4 bytes"));
    assert_eq!(width_field as usize, width);
    // The first stored row, blue, green and red a pixel: black where the view hides the
    // margins, the white backdrop elsewhere.
    let row: String = bmp[54..54 + width * 3]
        .chunks(3)
        .map(|pixel| match pixel {
            [0, 0, 0] => '.',
            [0xFF, 0xFF, 0xFF] => 'w',
            _ => '?',
        })
        .collect();
    let hidden = WIDE_MARGIN - 8;
    let expected = format!(
        "{}{}{}",
        ".".repeat(hidden),
        "w".repeat(width - WIDE_MARGIN - hidden),
        ".".repeat(WIDE_MARGIN)
    );
    assert_eq!(row, expected);
    fs::remove_dir_all(&folder).expect("the test folder is removed");
}
