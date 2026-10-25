//! The top-level loop, return-check cascades, the interpreter fallback, the run limits and
//! the native-depth guard, on synthetic functions written in the emitter's shape: each
//! takes the address to start at, returns where execution continues, compares the result of
//! every call with the return address it pushed, and calls `ag_check` at loop targets.

use anygenesis_abi::{AgEntry, AgState, ram_index};
use anygenesis_rom::Rom;
use anygenesis_runtime::{
    Exit, Miss, Options, Runtime, StubVdp, UNWIND, ag_check, ag_lookup, ag_stop,
};

const SSP: u32 = 0x00FF_FE00;

/// Gives `body` the state for one short access, so no borrow lives across a call.
fn with<T>(state: *mut AgState, body: impl FnOnce(&mut AgState) -> T) -> T {
    // SAFETY: the runtime passes a live state, and every caller drops the borrow before it
    // calls anything that derives its own.
    unsafe { body(&mut *state) }
}

fn push(state: *mut AgState, value: u32) {
    with(state, |state| {
        state.a[7] = state.a[7].wrapping_sub(4);
        for (offset, byte) in value.to_be_bytes().into_iter().enumerate() {
            state.ram[ram_index(state.a[7] + offset as u32)] = byte;
        }
    });
}

fn pop(state: *mut AgState) -> u32 {
    with(state, |state| {
        let bytes = std::array::from_fn(|offset| state.ram[ram_index(state.a[7] + offset as u32)]);
        state.a[7] = state.a[7].wrapping_add(4);
        u32::from_be_bytes(bytes)
    })
}

/// `jsr`: pushes `ret`, calls, and returns the result when it is not `ret`.
fn call(state: *mut AgState, target: u32, ret: u32) -> Option<u32> {
    push(state, ret);
    // SAFETY: the state of a running run; `ag_lookup` never returns null.
    let next = unsafe { ag_lookup(state, target)(state, target) };
    (next != ret).then_some(next)
}

/// `AG_CYCLES` then `AG_CHECK` at a loop target.
fn check(state: *mut AgState, pc: u32, cost: i32) -> Option<u32> {
    let due = with(state, |state| {
        state.cycles -= cost;
        state.cycles <= 0
    });
    if !due {
        return None;
    }
    // SAFETY: the state of a running run.
    let next = unsafe { ag_check(state, pc) };
    (next != pc).then_some(next)
}

/// STOP, continuing at `resume`.
fn stop(state: *mut AgState, resume: u32) -> Option<u32> {
    // SAFETY: the state of a running run.
    let next = unsafe { ag_stop(state, resume) };
    (next != resume).then_some(next)
}

fn set(state: *mut AgState, body: impl FnOnce(&mut AgState)) {
    with(state, body);
}

/// main: `d0 = 1; jsr $300; d0 = 2; stop; loop: bra loop`.
unsafe extern "C" fn f_000200(state: *mut AgState, pc: u32) -> u32 {
    if pc == 0x200 {
        set(state, |state| state.d[0] = 1);
        if let Some(next) = call(state, 0x300, 0x206) {
            return next;
        }
    }
    if pc <= 0x206 {
        set(state, |state| state.d[0] = 2);
        if let Some(next) = stop(state, 0x20A) {
            return next;
        }
    }
    loop {
        if let Some(next) = check(state, 0x20A, 10) {
            return next;
        }
    }
}

/// sub: `jsr (a0); rts`.
unsafe extern "C" fn f_000300(state: *mut AgState, pc: u32) -> u32 {
    if pc == 0x300 {
        let target = with(state, |state| state.a[0]);
        if let Some(next) = call(state, target, 0x304) {
            return next;
        }
    }
    pop(state)
}

/// Pops its caller's frame and returns to the grandparent: `addq.l #4,sp; rts`.
unsafe extern "C" fn f_000310(state: *mut AgState, _pc: u32) -> u32 {
    pop(state);
    pop(state)
}

/// Rewrites its return address: `move.l #$400,(sp); rts`.
unsafe extern "C" fn f_000320(state: *mut AgState, _pc: u32) -> u32 {
    pop(state);
    push(state, 0x400);
    pop(state)
}

/// Returns to code no function covers: `move.l #$540,(sp); rts`.
unsafe extern "C" fn f_000330(state: *mut AgState, _pc: u32) -> u32 {
    pop(state);
    push(state, 0x540);
    pop(state)
}

/// `d1 = $44; rts`.
unsafe extern "C" fn f_000400(state: *mut AgState, _pc: u32) -> u32 {
    set(state, |state| state.d[1] = 0x44);
    pop(state)
}

/// `d3 = $66; rts`.
unsafe extern "C" fn f_000600(state: *mut AgState, _pc: u32) -> u32 {
    set(state, |state| state.d[3] = 0x66);
    pop(state)
}

/// A caller that a popped callee abandons, over and over: `loop: jsr $710` where $710 is
/// `addq.l #4,sp; jmp loop`. Every round leaves a native frame behind.
unsafe extern "C" fn f_000700(state: *mut AgState, _pc: u32) -> u32 {
    if let Some(next) = check(state, 0x700, 20) {
        return next;
    }
    push(state, 0x706);
    // SAFETY: as for every synthetic function.
    let next = unsafe { f_000710(state, 0x710) };
    if next != 0x706 {
        return next;
    }
    pop(state)
}

unsafe extern "C" fn f_000710(state: *mut AgState, _pc: u32) -> u32 {
    pop(state);
    // SAFETY: as for every synthetic function.
    unsafe { f_000700(state, 0x700) }
}

/// `loop: addq.l #1,d0; bra loop`.
unsafe extern "C" fn f_000800(state: *mut AgState, _pc: u32) -> u32 {
    loop {
        if let Some(next) = check(state, 0x800, 8) {
            return next;
        }
        set(state, |state| state.d[0] += 1);
    }
}

static TABLE: [AgEntry; 12] = [
    AgEntry {
        address: 0x200,
        function: f_000200,
    },
    AgEntry {
        address: 0x206,
        function: f_000200,
    },
    AgEntry {
        address: 0x20A,
        function: f_000200,
    },
    AgEntry {
        address: 0x300,
        function: f_000300,
    },
    AgEntry {
        address: 0x304,
        function: f_000300,
    },
    AgEntry {
        address: 0x310,
        function: f_000310,
    },
    AgEntry {
        address: 0x320,
        function: f_000320,
    },
    AgEntry {
        address: 0x330,
        function: f_000330,
    },
    AgEntry {
        address: 0x400,
        function: f_000400,
    },
    AgEntry {
        address: 0x600,
        function: f_000600,
    },
    AgEntry {
        address: 0x700,
        function: f_000700,
    },
    AgEntry {
        address: 0x800,
        function: f_000800,
    },
];

/// 68000 code the table does not cover, run by the interpreter.
const CODE: &[(u32, &[u16])] = &[
    // moveq #5,d1; jsr $600.l; moveq #7,d2; rts
    (0x500, &[0x7205, 0x4EB9, 0x0000, 0x0600, 0x7407, 0x4E75]),
    // moveq #9,d4; jmp $400.l
    (0x520, &[0x7809, 0x4EF9, 0x0000, 0x0400]),
    // moveq #3,d5; rts
    (0x540, &[0x7A03, 0x4E75]),
    // bra.s *
    (0x560, &[0x60FE]),
    // moveq #1,d6; stop #$2700
    (0x580, &[0x7C01, 0x4E72, 0x2700]),
];

fn rom() -> Rom {
    let mut bytes = vec![0; 0x1000];
    bytes[0..4].copy_from_slice(&SSP.to_be_bytes());
    bytes[4..8].copy_from_slice(&0x200u32.to_be_bytes());
    for &(address, words) in CODE {
        for (index, word) in words.iter().enumerate() {
            let at = address as usize + 2 * index;
            bytes[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }
    Rom::from_bytes(bytes).expect("the synthetic image is a well-formed .bin")
}

fn runtime(options: Options) -> Runtime {
    Runtime::new(rom(), &TABLE, Box::<StubVdp>::default(), options)
}

fn idle() -> Options {
    Options {
        stop_when_idle: true,
        ..Options::default()
    }
}

/// Runs main with `a0` as sub's call target; main stops after sub returns.
fn run_main(target: u32) -> Runtime {
    let mut runtime = runtime(Options::default());
    runtime.state_mut().a[0] = target;
    assert_eq!(runtime.run(), Exit::Stopped { resume: 0x20A });
    assert_eq!(runtime.state().d[0], 2);
    assert_eq!(runtime.state().a[7], SSP, "68000 stack balanced");
    runtime
}

fn misses(runtime: &Runtime) -> Vec<(u32, Miss)> {
    runtime
        .log()
        .misses()
        .iter()
        .map(|(&address, &miss)| (address, miss))
        .collect()
}

#[test]
fn test_reset_state_comes_from_the_vectors() {
    let runtime = runtime(Options::default());
    let state = runtime.state();
    assert_eq!((state.pc, state.a[7], state.sr), (0x200, SSP, 0x2700));
    assert_eq!(state.rom_size, 0x1000);
}

#[test]
fn test_popped_caller_cascades_to_the_grandparent_natively() {
    let runtime = run_main(0x310);
    assert!(misses(&runtime).is_empty());
    assert_eq!(runtime.log().unwinds(), 0);
}

#[test]
fn test_rewritten_return_reaches_the_top_level_and_resumes_through_the_table() {
    let runtime = run_main(0x320);
    assert_eq!(runtime.state().d[1], 0x44);
    assert!(misses(&runtime).is_empty());
}

#[test]
fn test_top_level_resume_without_a_row_runs_the_interpreter() {
    let runtime = run_main(0x330);
    assert_eq!(runtime.state().d[5], 3);
    let expected = Miss {
        resumes: 1,
        instructions: 2,
        ..Miss::default()
    };
    assert_eq!(misses(&runtime), [(0x540, expected)]);
}

#[test]
fn test_lookup_miss_interprets_and_calls_translated_code_natively() {
    let runtime = run_main(0x500);
    let state = runtime.state();
    assert_eq!((state.d[1], state.d[2], state.d[3]), (5, 7, 0x66));
    let expected = Miss {
        lookups: 1,
        instructions: 4,
        ..Miss::default()
    };
    assert_eq!(misses(&runtime), [(0x500, expected)]);
}

#[test]
fn test_interpreter_hands_back_at_the_first_table_address() {
    let runtime = run_main(0x520);
    assert_eq!((runtime.state().d[4], runtime.state().d[1]), (9, 0x44));
    let expected = Miss {
        lookups: 1,
        instructions: 2,
        ..Miss::default()
    };
    assert_eq!(misses(&runtime), [(0x520, expected)]);
}

#[test]
fn test_run_continues_after_stop_and_ends_at_an_idle_loop() {
    let mut runtime = runtime(idle());
    runtime.state_mut().a[0] = 0x310;
    assert_eq!(runtime.run(), Exit::Stopped { resume: 0x20A });
    assert_eq!(runtime.run(), Exit::Idle { pc: 0x20A });
}

#[test]
fn test_busy_loop_is_not_idle_and_ends_at_the_cycle_cap() {
    let mut runtime = runtime(Options {
        max_cycles: 100_000,
        ..idle()
    });
    runtime.state_mut().pc = 0x800;
    assert_eq!(runtime.run(), Exit::CycleCap { pc: 0x800 });
    assert!(runtime.cycles() >= 100_000);
    assert!(runtime.state().d[0] > 10_000);
}

#[test]
fn test_interpreted_loop_reaches_check_points_and_goes_idle() {
    let mut runtime = runtime(idle());
    runtime.state_mut().pc = 0x560;
    assert_eq!(runtime.run(), Exit::Idle { pc: 0x560 });
    assert_eq!(runtime.log().misses()[&0x560].resumes, 1);
}

#[test]
fn test_interpreted_stop_ends_the_run() {
    let mut runtime = runtime(Options::default());
    runtime.state_mut().pc = 0x580;
    assert_eq!(runtime.run(), Exit::Stopped { resume: 0x586 });
    assert_eq!(runtime.state().d[6], 1);
    assert_eq!(runtime.state().pc, 0x586);
}

#[test]
fn test_depth_guard_unwinds_abandoned_native_frames() {
    // A thread with a known stack, so the limits mean the same on every host.
    let worker = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| {
            let mut runtime = runtime(Options {
                max_cycles: 2_000_000,
                soft_depth: 256 << 10,
                hard_depth: 8 << 20,
                ..Options::default()
            });
            runtime.state_mut().pc = 0x700;
            let exit = runtime.run();
            (exit, runtime.log().unwinds(), runtime.state().a[7])
        })
        .expect("the test thread starts");
    let (exit, unwinds, sp) = worker.join().expect("the guarded run finishes");
    assert_eq!(exit, Exit::CycleCap { pc: 0x700 });
    assert!(unwinds > 10, "{unwinds} unwinds");
    assert_eq!(sp, SSP);
}

#[test]
fn test_unwind_marker_matches_no_return_address() {
    assert_eq!(UNWIND & 0xFF_FFFF, 0);
}
