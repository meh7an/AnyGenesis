//! Interrupt delivery on interpreted 68000 programs: order, nesting, the interrupt mask,
//! STOP, NTSC frame timing on the VDP and the time a DMA takes from the 68000. No table
//! rows exist, so every instruction runs in the interpreter, whose instruction boundaries
//! are check points.

mod common;

use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Exit, Hardware, Options, Runtime, Vdp, refresh,
    timing::{FRAME_CLOCKS, LINE_CLOCKS},
};

use crate::common::{SSP, Scripted, image};

const HINT_VECTOR: u8 = 28;
const VINT_VECTOR: u8 = 30;
/// Where handlers append their marks, through A5.
const LOG: u32 = 0xFF_0000;

/// Level 4 and level 6 handlers that mark their start and end and spend about 1,000 cycles
/// in between, each counting in its own register so a nested one leaves the other alone:
/// `move.b #N,(a5)+; moveq #100,dn; dbf dn,*; move.b #$1N,(a5)+; rte`.
const HANDLERS: [(u32, &[u16]); 2] = [
    (
        0x300,
        &[
            0x1AFC, 0x0004, 0x7264, 0x51C9, 0xFFFE, 0x1AFC, 0x0014, 0x4E73,
        ],
    ),
    (
        0x380,
        &[
            0x1AFC, 0x0006, 0x7464, 0x51CA, 0xFFFE, 0x1AFC, 0x0016, 0x4E73,
        ],
    ),
];

/// `move #$2000,sr; loop: addq.l #1,d0; bra loop`.
const OPEN_LOOP: &[u16] = &[0x46FC, 0x2000, 0x5280, 0x60FC];

/// A program at $200 with the logging handlers.
fn program(main: &[u16]) -> Rom {
    let vectors = [(HINT_VECTOR, 0x300), (VINT_VECTOR, 0x380)];
    let code = [(0x200, main), HANDLERS[0], HANDLERS[1]];
    image(0x200, &vectors, &code)
}

fn scripted(main: &[u16], events: &[(u64, u8)], max_cycles: u64) -> (Runtime, Exit) {
    let options = Options {
        max_cycles,
        ..Options::default()
    };
    let mut runtime = Runtime::new(program(main), &[], Scripted::new(events), options);
    runtime.state_mut().a[5] = LOG;
    let exit = runtime.run();
    (runtime, exit)
}

/// The marks the handlers and the program left.
fn marks(runtime: &Runtime) -> Vec<u8> {
    let state = runtime.state();
    let written = (state.a[5] - LOG) as usize;
    state.ram[..written].to_vec()
}

#[test]
fn test_interrupts_arrive_in_time_order() {
    let events = [(1_000, 4), (3_000, 6), (5_000, 4)];
    let (runtime, exit) = scripted(OPEN_LOOP, &events, 7_000);
    assert!(matches!(exit, Exit::CycleCap { .. }), "{exit}");
    assert_eq!(marks(&runtime), [4, 0x14, 6, 0x16, 4, 0x14]);
    assert_eq!((runtime.interrupts(4), runtime.interrupts(6)), (2, 1));
    let state = runtime.state();
    assert_eq!((state.sr, state.a[7]), (0x2000, SSP));
}

#[test]
fn test_higher_level_nests_inside_a_lower_handler() {
    let (runtime, _) = scripted(OPEN_LOOP, &[(1_000, 4), (1_300, 6)], 4_000);
    assert_eq!(marks(&runtime), [4, 6, 0x16, 0x14]);
    assert_eq!(runtime.state().a[7], SSP);
}

#[test]
fn test_lower_level_waits_for_a_higher_handler() {
    let (runtime, _) = scripted(OPEN_LOOP, &[(1_000, 6), (1_300, 4)], 4_000);
    assert_eq!(marks(&runtime), [6, 0x16, 4, 0x14]);
}

#[test]
fn test_lowered_mask_lets_pending_interrupts_in_one_by_one() {
    // Both arrive under SR $2700; $2500 lets level 6 in before the next instruction,
    // $2000 then level 4.
    let main = [
        0x7664, 0x51CB, 0xFFFE, // moveq #100,d3; dbf d3,*
        0x46FC, 0x2500, // move #$2500,sr
        0x1AFC, 0x0001, // move.b #1,(a5)+
        0x46FC, 0x2000, // move #$2000,sr
        0x1AFC, 0x0002, // move.b #2,(a5)+
        0x60FE, // bra.s *
    ];
    let (runtime, _) = scripted(&main, &[(100, 4), (200, 6)], 5_000);
    assert_eq!(marks(&runtime), [6, 0x16, 1, 4, 0x14, 2]);
}

#[test]
fn test_stop_waits_for_an_interrupt_and_continues_after_it() {
    // stop #$2000; move.b #1,(a5)+; bra.s *
    let main = [0x4E72, 0x2000, 0x1AFC, 0x0001, 0x60FE];
    let (runtime, exit) = scripted(&main, &[(5_000, 6)], 8_000);
    assert_eq!(exit, Exit::CycleCap { pc: 0x208 });
    assert_eq!(marks(&runtime), [6, 0x16, 1]);
    assert!(!runtime.stopped());
}

#[test]
fn test_stop_with_every_raised_level_masked_ends_the_run() {
    let main = [0x4E72, 0x2700, 0x60FE];
    let (runtime, exit) = scripted(&main, &[(5_000, 6)], 8_000);
    assert_eq!(exit, Exit::Stopped { resume: 0x204 });
    assert!(runtime.stopped());
    assert!(marks(&runtime).is_empty());
}

/// Turns on the vertical interrupt, the horizontal one every 10 lines, opens the mask and
/// idles; the handlers count in RAM.
fn frame_program() -> Rom {
    let main: &[u16] = &[
        0x33FC, 0x8164, 0x00C0, 0x0004, // move.w #$8164,$C00004: display, VInt on
        0x33FC, 0x8014, 0x00C0, 0x0004, // move.w #$8014,$C00004: HInt on
        0x33FC, 0x8A09, 0x00C0, 0x0004, // move.w #$8A09,$C00004: every 10 lines
        0x46FC, 0x2000, // move #$2000,sr
        0x60FE, // bra.s *
    ];
    // addq.w #1,$FF0012 (HInt) or $FF0010 (VInt); rte
    let hint: &[u16] = &[0x5279, 0x00FF, 0x0012, 0x4E73];
    let vint: &[u16] = &[0x5279, 0x00FF, 0x0010, 0x4E73];
    let vectors = [(HINT_VECTOR, 0x300), (VINT_VECTOR, 0x380)];
    image(
        0x200,
        &vectors,
        &[(0x200, main), (0x300, hint), (0x380, vint)],
    )
}

fn vdp_runtime(options: Options) -> Runtime {
    let options = Options {
        // No TMSS, so the program may touch the VDP at once.
        version: 0xA0,
        ..options
    };
    Runtime::new(frame_program(), &[], Box::<Vdp>::default(), options)
}

/// The VBlank and HBlank handler counts.
fn counts(runtime: &Runtime) -> (u16, u16) {
    let ram = &runtime.state().ram;
    (
        u16::from_be_bytes([ram[0x10], ram[0x11]]),
        u16::from_be_bytes([ram[0x12], ram[0x13]]),
    )
}

#[test]
fn test_vertical_interrupt_comes_once_per_frame() {
    let mut runtime = vdp_runtime(Options::default());
    assert_eq!(runtime.run_frames(60), Exit::Frames { pc: 0x21C });
    assert_eq!(runtime.frames(), 60);
    // The run ends where the 60th frame does, before its interrupt is taken.
    assert_eq!(runtime.interrupts(6), 59);
    assert_eq!(counts(&runtime).0, 59);
    let clock = runtime.cycles() * 7;
    let end = 59 * FRAME_CLOCKS + 224 * LINE_CLOCKS;
    assert!(
        (end..end + 100 * 7).contains(&clock),
        "ended at master clock {clock}, frame end {end}"
    );
    runtime.run_frames(1);
    assert_eq!(runtime.interrupts(6), 60);
}

#[test]
fn test_horizontal_interrupt_comes_every_reload_plus_one_lines() {
    let mut runtime = vdp_runtime(Options::default());
    runtime.run_frames(2);
    let (vblanks, hblanks) = counts(&runtime);
    runtime.run_frames(10);
    let (later_vblanks, later_hblanks) = counts(&runtime);
    assert_eq!(later_vblanks - vblanks, 10);
    // Lines 9, 19, ... 219 of each picture.
    assert_eq!(later_hblanks - hblanks, 10 * 22);
}

#[test]
fn test_frames_last_their_master_clocks() {
    let mut runtime = vdp_runtime(Options::default());
    runtime.run_frames(1);
    let start = runtime.cycles();
    runtime.run_frames(10);
    let clocks = (runtime.cycles() - start) * 7;
    let expected = 10 * FRAME_CLOCKS;
    assert!(
        clocks.abs_diff(expected) < 100 * 7,
        "{clocks} master clocks for 10 frames"
    );
    let vdp = runtime.hardware::<Vdp>().expect("the hardware is the VDP");
    assert_eq!(vdp.frames(), 11);
}

#[test]
fn test_idle_loop_waiting_for_an_enabled_interrupt_is_not_a_hang() {
    let mut runtime = vdp_runtime(Options {
        stop_when_idle: true,
        ..Options::default()
    });
    assert_eq!(runtime.run_frames(5), Exit::Frames { pc: 0x21C });
}

/// Sets register 1 to `mode` and asks for a 68K-to-VRAM DMA of 0x400 words from ROM 0,
/// then counts in D0 until the cycle cap.
fn dma_program(mode: u16) -> Rom {
    let main: &[u16] = &[
        0x4BF9, 0x00C0, 0x0004, // lea $C00004,a5
        0x3ABC, mode, // move.w #mode,(a5)
        0x3ABC, 0x8F02, // move.w #$8F02,(a5): increment 2
        0x3ABC, 0x9300, // move.w #$9300,(a5): length 0x0400 words
        0x3ABC, 0x9404, // move.w #$9404,(a5)
        0x3ABC, 0x9500, // move.w #$9500,(a5): source 0
        0x3ABC, 0x9600, // move.w #$9600,(a5)
        0x3ABC, 0x9700, // move.w #$9700,(a5)
        0x2ABC, 0x4000, 0x0080, // move.l #$40000080,(a5): VRAM write at 0, DMA
        0x5280, // loop: addq.l #1,d0
        0x60FC, // bra.s loop
    ];
    image(0x200, &[], &[(0x200, main)])
}

fn run_dma_program(mode: u16) -> Runtime {
    let options = Options {
        version: 0xA0,
        max_cycles: 50_000,
        ..Options::default()
    };
    let mut runtime = Runtime::new(dma_program(mode), &[], Box::<Vdp>::default(), options);
    runtime.run();
    runtime
}

#[test]
fn test_dma_from_68000_memory_freezes_the_program_for_its_duration() {
    let with_dma = run_dma_program(0x8114);
    let without = run_dma_program(0x8104);
    let vdp = with_dma.hardware::<Vdp>().expect("the hardware is the VDP");
    assert_eq!(vdp.counts().transfers, 1);
    assert_eq!(vdp.vram()[..0x800], with_dma.rom().bytes()[..0x800]);
    assert!(with_dma.log().accesses().is_empty());
    // 0x800 VRAM bytes at 167 a blanked H32 line: about 12 lines frozen.
    let stall = vdp.counts().stall_cycles;
    assert!((5_900..6_100).contains(&stall), "{stall} cycles");
    // The loop takes 18 cycles a turn and refresh 2 per 128 of them, while the stall causes
    // none, so it turns stall / (18 * 130 / 128) fewer times.
    let turns = |runtime: &Runtime| u64::from(runtime.state().d[0]);
    let lost = turns(&without) - turns(&with_dma);
    let interval = u64::from(refresh::INTERVAL);
    let expected = stall * interval / (18 * (interval + u64::from(refresh::DELAY)));
    assert!(lost.abs_diff(expected) <= 2, "{lost} turns lost");
    // The VDP's clock moved on by the stall like every other cycle.
    assert_eq!(vdp.timing().clock(), with_dma.cycles() * 7);
}
