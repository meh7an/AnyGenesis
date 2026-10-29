//! The Z80 on the bus (M3.2): interpreted 68000 programs load a Z80 program the way games
//! do (bus request, reset released, bytes copied, bus released), and the Z80 then runs on
//! the scheduler's clock: its reads through the bank window cost the 68000 cycles, and the
//! VDP interrupts it once a frame.

mod common;

use anygenesis_rom::Rom;
use anygenesis_runtime::{Exit, Options, Runtime, Vdp, z80::MAIN_STEAL};

use crate::common::image;

/// No TMSS, so programs may touch the VDP at once.
const NO_TMSS: u8 = 0xA0;
/// Where the Z80 program sits in the ROM image.
const Z80_PROGRAM: u32 = 0x800;

/// The ROM's 68000 program: loads the `length` bytes of the Z80 program, lets the Z80 run,
/// then counts in D0 forever.
fn loader(length: usize) -> Vec<u16> {
    let count = u16::try_from(length - 1).expect("a short Z80 program");
    vec![
        0x33FC, 0x0100, 0x00A1, 0x1100, // move.w #$100,$A11100: request the bus
        0x33FC, 0x0100, 0x00A1, 0x1200, // move.w #$100,$A11200: release reset
        0x41F9, 0x0000, 0x0800, // lea $800,a0
        0x43F9, 0x00A0, 0x0000, // lea $A00000,a1
        0x323C, count,  // move.w #count,d1
        0x12D8, // copy: move.b (a0)+,(a1)+
        0x51C9, 0xFFFC, // dbf d1,copy
        0x33FC, 0x0000, 0x00A1, 0x1100, // move.w #0,$A11100: release the bus
        0x5280, // count: addq.l #1,d0
        0x60FC, // bra.s count
    ]
}

/// A ROM with the loader at $200 and `z80` at [`Z80_PROGRAM`].
fn rom_with(z80: &[u8]) -> Rom {
    let words: Vec<u16> = z80
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]))
        .collect();
    image(
        0x200,
        &[],
        &[(0x200, &loader(z80.len())), (Z80_PROGRAM, &words)],
    )
}

fn runtime(rom: Rom, max_cycles: u64) -> Runtime {
    let options = Options {
        version: NO_TMSS,
        max_cycles,
        ..Options::default()
    };
    Runtime::new(rom, &[], Box::<Vdp>::default(), options)
}

#[test]
fn test_each_bank_window_read_costs_the_68000_its_cycles() {
    // LD B,100; then 100 times LD A,(0x8000), a ROM read through the window; HALT.
    let reads = 100;
    let busy = [0x06, reads, 0x3A, 0x00, 0x80, 0x10, 0xFB, 0x76];
    // HALT at once, padded so the 68000 copies as many bytes.
    let idle = [0x76, 0, 0, 0, 0, 0, 0, 0];
    let counted = |program: &[u8]| {
        let mut runtime = runtime(rom_with(program), 60_000);
        assert!(matches!(runtime.run(), Exit::CycleCap { .. }));
        assert!(runtime.z80().cpu().halted);
        assert!(runtime.log().accesses().is_empty());
        runtime.state().d[0]
    };
    let lost = u64::from(counted(&idle) - counted(&busy)) * 18;
    let stolen = u64::from(reads) * u64::from(MAIN_STEAL);
    // One turn of the 68000's loop is 18 cycles; refresh blurs it by a turn.
    assert!(
        lost.abs_diff(stolen) <= 2 * 18,
        "lost {lost}, stolen {stolen}"
    );
}

#[test]
fn test_the_vdp_interrupts_the_z80_once_a_frame() {
    // IM 1 with a handler that counts in RAM 0x1000 and outlasts the line the interrupt
    // stays asserted for, so each frame counts once.
    let mut program = vec![0x31, 0x00, 0x20, 0xED, 0x56, 0xFB, 0x18, 0xFE];
    program.resize(0x38, 0);
    program.extend([
        0x3A, 0x00, 0x10, // LD A,(0x1000)
        0x3C, // INC A
        0x32, 0x00, 0x10, // LD (0x1000),A
        0x06, 0x14, // LD B,20
        0x10, 0xFE, // DJNZ to itself: 260 T-states, longer than a line's 228
        0xFB, // EI
        0xC9, // RET
    ]);
    let mut runtime = runtime(rom_with(&program), u64::MAX);
    assert!(matches!(runtime.run_frames(10), Exit::Frames { .. }));
    // The tenth interrupt comes with the end of the tenth frame, where the run ended.
    assert_eq!(runtime.z80_ram()[0x1000], 9);
    runtime.run_frames(1);
    assert_eq!(runtime.z80_ram()[0x1000], 10);
}
