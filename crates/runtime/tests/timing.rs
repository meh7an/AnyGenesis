//! The synthetic timing suite (M1.2-M1.4): interpreted 68000 programs on the scheduler and
//! the VDP. Interrupts land at the end of the first instruction that latches them, or right
//! after the SR write that unmasks one, budgets
//! end where an enabled interrupt or a waited-for frame end falls, a register write that
//! enables an interrupt cuts the budget, and the FIFO stalls data-port writes while the
//! display draws. The FIFO's exact slot arithmetic is tested in the VDP's own unit tests.
//! A register, CRAM or VSRAM write from a horizontal interrupt handler shows from the line
//! after the one the counter ran out on, as games use it for split effects.

mod common;

use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Exit, Options, RamPatch, RamWhen, Runtime, Vdp, refresh,
    timing::{CPU_DIVIDER, FRAME_CLOCKS, LINE_CLOCKS},
    vdp::{Frame, render::HEIGHT},
};

use crate::common::{Scripted, image};

const NMI_VECTOR: u8 = 31;
const HINT_VECTOR: u8 = 28;
const VINT_VECTOR: u8 = 30;

/// No TMSS, so programs may touch the VDP at once.
const NO_TMSS: u8 = 0xA0;

fn vdp_runtime(rom: Rom, options: Options) -> Runtime {
    let options = Options {
        version: NO_TMSS,
        ..options
    };
    Runtime::new(rom, &[], Box::<Vdp>::default(), options)
}

fn word(ram: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([ram[at], ram[at + 1]])
}

/// The hardware time after `executed` cycles from power-on: refresh holds the 68000 two
/// cycles in every 128 it executes.
fn with_refresh(executed: u64) -> u64 {
    let interval = u64::from(refresh::INTERVAL);
    executed + executed / interval * u64::from(refresh::DELAY)
}

/// An interrupt handler for $E00: stores its stacked PC at RAM $FF0000 and waits.
const STORE_STACKED_PC: &[u16] = &[
    0x23EF, 0x0002, 0x00FF, 0x0000, // move.l 2(sp),$FF0000
    0x60FE, // bra.s *
];

/// The PC stacked by the interrupt `level` raised at cycle `event` while `program` runs from
/// $200.
fn stacked_pc(program: &[u16], vector: u8, event: u64, level: u8) -> u32 {
    let rom = image(
        0x200,
        &[(vector, 0xE00)],
        &[(0x200, program), (0xE00, STORE_STACKED_PC)],
    );
    let options = Options {
        max_cycles: 3_000,
        ..Options::default()
    };
    let mut runtime = Runtime::new(rom, &[], Scripted::new(&[(event, level)]), options);
    runtime.run();
    let ram = &runtime.state().ram;
    u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]])
}

#[test]
fn test_interrupt_waits_for_an_instruction_that_latches_it() {
    // NOPs (4 cycles) latch at their start: an event inside one is taken at the end of the
    // next, the one instruction of latency measured on hardware.
    let nops = [0x4E71; 400];
    for event in [1_000, 1_001, 1_234] {
        let nop = (0..)
            .find(|k| with_refresh(4 * k) >= event)
            .expect("a boundary");
        let stacked = stacked_pc(&nops, NMI_VECTOR, event, 7);
        assert_eq!(
            stacked,
            0x200 + 2 * (nop + 1) as u32,
            "event at cycle {event}"
        );
    }
}

#[test]
fn test_interrupt_before_the_operand_read_is_taken_at_that_instruction_end() {
    // tst.b ($0100).w (12 cycles) latches after its operand read, 8 cycles in.
    let tests: Vec<u16> = [0x4A38, 0x0100].repeat(100);
    for event in [600, 601, 605, 608, 609, 611] {
        let tst = (0..)
            .find(|k| with_refresh(12 * k + 8) >= event)
            .expect("a latch");
        let stacked = stacked_pc(&tests, NMI_VECTOR, event, 7);
        assert_eq!(
            stacked,
            0x200 + 4 * (tst + 1) as u32,
            "event at cycle {event}"
        );
    }
}

#[test]
fn test_interrupt_waiting_behind_the_mask_is_taken_right_after_the_sr_write() {
    // Masked from reset; the VInt comes during the NOPs, the MOVE to SR at $214 opens the
    // mask and the interrupt is taken before the next instruction.
    let mut program = vec![0x4E71; 10];
    program.extend([0x46FC, 0x2000]);
    program.extend([0x4E71; 100]);
    assert_eq!(stacked_pc(&program, VINT_VECTOR, 12, 6), 0x218);
}

/// Sets H40 with the display on and the line counter to `reload`, idles, then turns the
/// horizontal interrupt on and opens the mask; the handler counts in RAM $FF0010.
fn hint_program(reload: u8) -> Rom {
    let counter = 0x8A00 | u16::from(reload);
    let main: &[u16] = &[
        0x4BF9, 0x00C0, 0x0004, // lea $C00004,a5
        0x3ABC, 0x8C81, // move.w #$8C81,(a5): H40
        0x3ABC, 0x8144, // move.w #$8144,(a5): display on, VInt off
        0x3ABC, counter, // move.w #$8Axx,(a5): line counter reload
        0x46FC, 0x2000, // move #$2000,sr
        0x323C, 0x0400, // move.w #$400,d1
        0x51C9, 0xFFFE, // dbf d1,*: about 10,000 cycles with HInt off
        0x3ABC, 0x8014, // move.w #$8014,(a5): HInt on
        0x60FE, // bra.s *
    ];
    // addq.w #1,$FF0010; rte
    let hint: &[u16] = &[0x5279, 0x00FF, 0x0010, 0x4E73];
    image(
        0x200,
        &[(HINT_VECTOR, 0x300)],
        &[(0x200, main), (0x300, hint)],
    )
}

#[test]
fn test_horizontal_interrupt_comes_on_every_line_the_counter_runs_out_on() {
    // Budgets end only where the counter runs out, not at every line's step.
    let lines = 60;
    let cap = lines * LINE_CLOCKS / CPU_DIVIDER;
    let options = Options {
        max_cycles: cap,
        ..Options::default()
    };
    let mut runtime = vdp_runtime(hint_program(0), options);
    runtime.run();
    // The counter ran out on every line while disabled, so one interrupt is pending when it
    // is enabled; after that, one a line until the cap.
    let enabled_at = 10_000 * CPU_DIVIDER / LINE_CLOCKS;
    let taken = runtime.interrupts(4);
    let expected = lines - enabled_at;
    assert!(
        taken.abs_diff(expected) <= 2,
        "{taken} HInts, {expected} lines after the enable"
    );
    // The cap may fall inside the last interrupt's entry, before its handler counts it.
    let counted = u64::from(word(&runtime.state().ram, 0x10));
    assert!(taken - counted <= 1, "{taken} taken, {counted} counted");
}

#[test]
fn test_vertical_interrupt_enabled_mid_budget_arrives_at_its_hv_position() {
    // H40, display and VInt on; the handler stores the HV counter as its first access. No
    // interrupt is enabled and no frame limit set when the first budget starts, so it runs
    // to the cycle cap: the enabling write must cut it at the VInt point.
    let main: &[u16] = &[
        0x4BF9, 0x00C0, 0x0004, // lea $C00004,a5
        0x3ABC, 0x8C81, // move.w #$8C81,(a5)
        0x3ABC, 0x8164, // move.w #$8164,(a5)
        0x46FC, 0x2000, // move #$2000,sr
        0x60FE, // bra.s *: 10 cycles
    ];
    let vint: &[u16] = &[
        0x33F9, 0x00C0, 0x0008, 0x00FF, 0x0000, // move.w $C00008,$FF0000
        0x4E73, // rte
    ];
    let rom = image(
        0x200,
        &[(VINT_VECTOR, 0x300)],
        &[(0x200, main), (0x300, vint)],
    );
    let options = Options {
        max_cycles: 120_000,
        ..Options::default()
    };
    let mut runtime = vdp_runtime(rom, options);
    runtime.run();
    assert_eq!(runtime.interrupts(6), 1);
    let hv = word(&runtime.state().ram, 0);
    // V counter 224 ($E0). H: the VInt point is H $01 (16 master clocks into the line); the
    // BRA it falls in latched before it, so the next BRA ends 10-20 cycles later, then 49-58
    // cycles of interrupt entry with the acknowledge waiting for E, then the MOVE reads at
    // its start: 16 + 70..140 + 343..406 master clocks, plus refresh, at 16 a step.
    assert_eq!(hv >> 8, 0xE0, "{hv:04X}");
    assert!((0x1A..=0x24).contains(&(hv & 0xFF)), "{hv:04X}");
}

#[test]
fn test_frame_limit_ends_the_run_at_its_frame_with_every_interrupt_off() {
    // move #$2000,sr; bra.s *
    let rom = image(0x200, &[], &[(0x200, &[0x46FC, 0x2000, 0x60FE])]);
    let mut runtime = vdp_runtime(rom, Options::default());
    assert_eq!(runtime.run_frames(3), Exit::Frames { pc: 0x204 });
    assert_eq!(runtime.frames(), 3);
    // H32 at power-on: the frame point is 20 master clocks into line 224.
    let end = 2 * FRAME_CLOCKS + 224 * LINE_CLOCKS + 20;
    let clock = runtime.cycles() * CPU_DIVIDER;
    assert!(
        (end..end + 20 * CPU_DIVIDER).contains(&clock),
        "ended at master clock {clock}, frame end {end}"
    );
}

/// The horizontal interrupt's line counter in the line-change tests: interrupts at the end
/// of lines 99 and 199.
const CHANGE_RELOAD: u16 = 99;

/// H40 with both planes made of pattern 0, whose rows 0-3 are CRAM index 1 (red) and rows
/// 4-7 index 2 (green); auto-increment 0 and the data port left on `command`. The horizontal
/// interrupt runs `change`, the vertical one `undo`, then each returns. Gives the frame
/// finished after three frames.
fn line_change_frame(command: u32, change: &[u16], undo: &[u16]) -> Frame {
    let [high, low] = [(command >> 16) as u16, command as u16];
    let counter = 0x8A00 | CHANGE_RELOAD;
    let main: &[u16] = &[
        0x4BF9, 0x00C0, 0x0004, // lea $C00004,a5
        0x4DF9, 0x00C0, 0x0000, // lea $C00000,a6
        0x3ABC, 0x8C81, // move.w #$8C81,(a5): H40
        0x3ABC, 0x8F02, // move.w #$8F02,(a5): increment 2
        0x3ABC, 0x8230, // move.w #$8230,(a5): plane A at $C000
        0x3ABC, 0x8407, // move.w #$8407,(a5): plane B at $E000
        0x3ABC, 0x856C, // move.w #$856C,(a5): sprites at $D800
        0x3ABC, 0x8D3F, // move.w #$8D3F,(a5): H-scroll at $FC00
        0x3ABC, counter, // move.w #$8Axx,(a5): line counter reload
        0x2ABC, 0x4000, 0x0000, // move.l #$40000000,(a5): VRAM write at 0
        0x7203, // moveq #3,d1
        0x2CBC, 0x1111, 0x1111, // move.l #$11111111,(a6)
        0x51C9, 0xFFF8, // dbf d1,*-6
        0x7203, // moveq #3,d1
        0x2CBC, 0x2222, 0x2222, // move.l #$22222222,(a6)
        0x51C9, 0xFFF8, // dbf d1,*-6
        0x2ABC, 0xC002, 0x0000, // move.l #$C0020000,(a5): CRAM write at index 1
        0x2CBC, 0x000E, 0x00E0, // move.l #$000E00E0,(a6)
        0x3ABC, 0x8F00, // move.w #$8F00,(a5): increment 0
        0x3ABC, 0x8164, // move.w #$8164,(a5): display and VInt on
        0x3ABC, 0x8014, // move.w #$8014,(a5): HInt on
        0x2ABC, high, low, // move.l #command,(a5)
        0x46FC, 0x2000, // move #$2000,sr
        0x60FE, // bra.s *
    ];
    // Each handler ends in rte.
    let [hint, vint] = [change, undo].map(|body| [body, &[0x4E73]].concat());
    let rom = image(
        0x200,
        &[(HINT_VECTOR, 0x300), (VINT_VECTOR, 0x400)],
        &[
            (0x200, main),
            (0x300, hint.as_slice()),
            (0x400, vint.as_slice()),
        ],
    );
    let mut runtime = vdp_runtime(rom, Options::default());
    runtime.run_frames(3);
    let vdp = runtime.hardware::<Vdp>().expect("the hardware is the VDP");
    vdp.frame().clone()
}

/// A CRAM word as 0x00RRGGBB with each 3-bit channel v at ladder step `level(v)` of 14.
fn ladder_rgb(word: u16, level: impl Fn(u32) -> u32) -> u32 {
    let channel = |shift: u16| level(u32::from(word >> shift & 7)) * 255 / 14;
    channel(1) << 16 | channel(5) << 8 | channel(9)
}

#[test]
fn test_changes_a_horizontal_interrupt_makes_show_from_the_next_line() {
    const RED: u16 = 0x000E;
    const GREEN: u16 = 0x00E0;
    const BLUE: u16 = 0x0E00;
    /// What the horizontal interrupt changes.
    #[derive(Debug, Clone, Copy)]
    enum Change {
        /// Register 12: shadow/highlight on, so the low-priority planes darken.
        ShadowHighlight,
        /// CRAM index 1: red to blue.
        Cram,
        /// VSRAM word 0: plane A scrolled four lines.
        Vsram,
    }
    let normal = |v| 2 * v;
    // Pattern 0's row on a line is (line + vertical scroll) % 8.
    let stripe = |row: usize| if row % 8 < 4 { RED } else { GREEN };
    for change in [Change::ShadowHighlight, Change::Cram, Change::Vsram] {
        // The data-port command, then the handlers' writes: the change and its undo.
        let (command, hint, vint): (u32, [u16; 2], [u16; 2]) = match change {
            // move.w #$8C89,(a5); move.w #$8C81,(a5)
            Change::ShadowHighlight => (0xC002_0000, [0x3ABC, 0x8C89], [0x3ABC, 0x8C81]),
            // move.w #$0E00,(a6); move.w #$000E,(a6)
            Change::Cram => (0xC002_0000, [0x3CBC, BLUE], [0x3CBC, RED]),
            // move.w #4,(a6); move.w #0,(a6)
            Change::Vsram => (0x4000_0010, [0x3CBC, 4], [0x3CBC, 0]),
        };
        let frame = line_change_frame(command, &hint, &vint);
        for line in 0..HEIGHT {
            let after = line > usize::from(CHANGE_RELOAD);
            let expected = match change {
                Change::ShadowHighlight if after => ladder_rgb(stripe(line), |v| v),
                Change::Cram if after && stripe(line) == RED => ladder_rgb(BLUE, normal),
                Change::Vsram if after => ladder_rgb(stripe(line + 4), normal),
                _ => ladder_rgb(stripe(line), normal),
            };
            assert!(
                frame.row(line).iter().all(|&pixel| pixel == expected),
                "{change:?}: line {line} {:06X}, expected {expected:06X}",
                frame.row(line)[0]
            );
        }
    }
}

/// Writes 40 VRAM words in a DBF loop (18 cycles a turn) with the display in `mode`
/// (register 1), H40.
fn fifo_program(mode: u16) -> Rom {
    let main: &[u16] = &[
        0x4BF9, 0x00C0, 0x0004, // lea $C00004,a5
        0x4DF9, 0x00C0, 0x0000, // lea $C00000,a6
        0x3ABC, 0x8C81, // move.w #$8C81,(a5): H40
        0x3ABC, mode, // move.w #mode,(a5)
        0x3ABC, 0x8F02, // move.w #$8F02,(a5): increment 2
        0x2ABC, 0x4000, 0x0000, // move.l #$40000000,(a5): VRAM write at 0
        0x7227, // moveq #39,d1
        0x3C81, // loop: move.w d1,(a6)
        0x51C9, 0xFFFC, // dbf d1,loop
        0x60FE, // bra.s *
    ];
    image(0x200, &[], &[(0x200, main)])
}

fn stall_cycles(mode: u16) -> u64 {
    let options = Options {
        max_cycles: 10_000,
        ..Options::default()
    };
    let mut runtime = vdp_runtime(fifo_program(mode), options);
    runtime.run();
    let vdp = runtime.hardware::<Vdp>().expect("the hardware is the VDP");
    assert_eq!(vdp.counts().data_writes, 40);
    assert_eq!(vdp.vram()[..4], [0, 39, 0, 38]);
    vdp.counts().stall_cycles
}

#[test]
fn test_fifo_stalls_vram_writes_while_the_display_draws() {
    // Drawn H40 lines leave 18 slots: a word every 380 master clocks (54 2/7 cycles) against
    // a write every 18. The last write is queued once the 36th has left, 36 words after
    // the first went out, and the loop's own 39 turns are the rest; where the first slot
    // falls and refresh move it by a slot or so.
    let drawn = stall_cycles(0x8144);
    let expected = 36 * (2 * 190) / CPU_DIVIDER - 39 * 18;
    assert!(
        drawn.abs_diff(expected) <= 30,
        "{drawn} cycles, about {expected}"
    );
    // Blanked: 205 slots a line, two of them in 34 master clocks, faster than the loop.
    assert_eq!(stall_cycles(0x8104), 0);
}

#[test]
fn test_ram_patches_land_at_reset_and_at_every_vblank_point() {
    // clr.b ($FF00).w over and over: only a patch can leave the byte set.
    let rom = image(0x200, &[], &[(0x200, &[0x4238, 0xFF00, 0x60FA])]);
    let patch = |address: u32, byte: u8, when: RamWhen| RamPatch {
        address,
        bytes: vec![byte],
        when,
    };
    let options = Options {
        ram_patches: vec![
            patch(0xFF_FF00, 0x5A, RamWhen::Vblank),
            patch(0xFF_FF02, 0x11, RamWhen::Reset),
            patch(0xFF_FFFF, 0x22, RamWhen::Vblank),
        ],
        ..Options::default()
    };
    let mut runtime = vdp_runtime(rom, options);
    let ram = |runtime: &Runtime, at: usize| runtime.state().ram[at];
    assert_eq!(
        ram(&runtime, 0xFF02),
        0x11,
        "reset, before the first instruction"
    );
    assert_eq!(ram(&runtime, 0xFF00), 0);
    for frame in 1..=3 {
        assert!(matches!(runtime.run_frames(1), Exit::Frames { .. }));
        // The run ends at the VBlank point, right after the patch.
        assert_eq!(ram(&runtime, 0xFF00), 0x5A, "frame {frame}");
        assert_eq!(ram(&runtime, 0xFFFF), 0x22, "frame {frame}");
        assert_eq!(runtime.frames(), frame);
    }
}
