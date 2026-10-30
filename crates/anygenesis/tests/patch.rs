//! A synthetic ROM patched by a pack that holds every ROM-level patch kind (an IPS file,
//! inline bytes, a Game Genie code) builds like any other ROM, with no hints: its analysis
//! finds the code only the patches make reachable. The whole-program build is ignored: it
//! needs the player library next to anygenesis, built in the same profile (`cargo build -p
//! anygenesis-player-lib`).

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

/// A well-formed image: every vector but the stack pointer at `RESET`, a made-up header
/// and a correct checksum.
fn synthetic_rom() -> Vec<u8> {
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
    rom[RESET..RESET + 2].copy_from_slice(&[0x60, 0xFE]);
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
    let rom = synthetic_rom();
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
