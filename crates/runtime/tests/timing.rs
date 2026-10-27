//! The synthetic timing suite (M1.2, M1.3): interpreted 68000 programs on the scheduler and
//! the VDP. Interrupts land at the end of the first instruction that latches them, or right
//! after the SR write that unmasks one, budgets
//! end where an enabled interrupt or a waited-for frame end falls, a register write that
//! enables an interrupt cuts the budget, and the FIFO stalls data-port writes while the
//! display draws. The FIFO's exact slot arithmetic is tested in the VDP's own unit tests.

mod common;

use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Exit, Options, Runtime, Vdp, refresh,
    timing::{CPU_DIVIDER, FRAME_CLOCKS, LINE_CLOCKS},
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
