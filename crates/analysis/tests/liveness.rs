//! Flag liveness over synthetic images, hand-assembled word by word: the live sets and
//! summaries of small programs, and liveness-on against liveness-off in the interpreter.

mod equivalence;

use anygenesis_analysis::{Analysis, FlagLiveness, LivenessOptions, analyze};
use anygenesis_ir::{Flag, FlagSet, Lifted, Op, flags};
use anygenesis_rom::Rom;
use anygenesis_shared::Address;

use equivalence::{Harness, Stop};

const ROM_SIZE: usize = 0x1000;
/// Every handler vector points at an `rte` here.
const HANDLER: u32 = 0x200;
const RESET: u32 = 0x300;
const F: u32 = 0x400;
const G: u32 = 0x500;
const H: u32 = 0x600;

const RTS: u16 = 0x4E75;
const RTE: u16 = 0x4E73;
const RTR: u16 = 0x4E77;
const NOP: u16 = 0x4E71;
/// moveq #0,d0
const MOVEQ_D0: u16 = 0x7000;
/// addq.w #1,d0
const ADDQ_D0: u16 = 0x5240;
/// addq.w #1,d2
const ADDQ_D2: u16 = 0x5242;
/// subq.w #1,d0
const SUBQ_D0: u16 = 0x5340;
/// cmp.w d1,d0
const CMP_D1_D0: u16 = 0xB041;
/// tst.w d0
const TST_D0: u16 = 0x4A40;
/// move.w d3,ccr: writes every flag, so nothing before it is live.
const MOVE_D3_CCR: u16 = 0x44C3;
/// move.w sr,d2
const MOVE_SR_D2: u16 = 0x40C2;
/// beq.s over one word
const BEQ_SKIP: u16 = 0x6702;
/// bcs.s over one word
const BCS_SKIP: u16 = 0x6502;
/// moveq #1,d2
const MOVEQ_D2: u16 = 0x7401;

const SEEDS: u64 = 64;
const CAP: u64 = 2_000;

/// A ROM image under construction.
struct Image {
    bytes: Vec<u8>,
}

impl Image {
    fn new() -> Self {
        let image = Self {
            bytes: vec![0; ROM_SIZE],
        };
        let image = (2..48).fold(image, |image, index| image.vector(index, HANDLER));
        image.vector(1, RESET).words(HANDLER, &[RTE])
    }

    fn vector(mut self, index: usize, value: u32) -> Self {
        self.bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        self
    }

    fn words(mut self, address: u32, words: &[u16]) -> Self {
        for (index, word) in words.iter().enumerate() {
            let offset = address as usize + index * 2;
            self.bytes[offset..offset + 2].copy_from_slice(&word.to_be_bytes());
        }
        self
    }

    fn analyze(&self) -> Analysis {
        analyze(&Rom::from_bytes(self.bytes.clone()).expect("synthetic image loads"))
    }
}

/// Returns `bsr.w target` placed at `site`.
fn bsr(site: u32, target: u32) -> [u16; 2] {
    [0x6100, target.wrapping_sub(site + 2) as u16]
}

/// Returns `bra.w target` placed at `site`.
fn bra(site: u32, target: u32) -> [u16; 2] {
    [0x6000, target.wrapping_sub(site + 2) as u16]
}

fn a(raw: u32) -> Address {
    Address::new(raw)
}

fn set_of(list: &[Flag]) -> FlagSet {
    list.iter().copied().collect()
}

/// Checks liveness on against liveness off for every function of `analysis`, from many
/// random states, and returns how the runs ended.
fn assert_equivalent(image: &Image, analysis: &Analysis, liveness: &FlagLiveness) -> Vec<Stop> {
    let mut harness = Harness::new(analysis, liveness, &image.bytes);
    let mut stops = Vec::new();
    for &entry in analysis.functions.keys() {
        let mut steps = 0;
        for seed in 0..SEEDS {
            match harness.run(entry, seed, CAP) {
                Ok(run) => {
                    steps += run.steps;
                    stops.push(run.stop);
                }
                Err(error) => panic!("function {entry:?}, seed {seed}: {error}"),
            }
        }
        assert!(steps >= SEEDS, "function {entry:?} barely ran");
    }
    stops
}

/// reset: bsr f; beq skip; moveq #1,d2; skip: move.w d3,ccr; rts
fn caller_reading_z(target: u32) -> Vec<u16> {
    let mut words = bsr(RESET, target).to_vec();
    words.extend([BEQ_SKIP, MOVEQ_D2, MOVE_D3_CCR, RTS]);
    words
}

#[test]
fn test_liveness_overwritten_flags_are_dead_and_caller_needs_only_z() {
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[MOVEQ_D0, ADDQ_D0, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    let summary = liveness.summary(a(F)).expect("f is a function");
    assert_eq!(summary.reads, FlagSet::EMPTY);
    assert_eq!(summary.defines, FlagSet::ALL);
    assert_eq!(summary.return_live, set_of(&[Flag::Z]));
    assert!(!summary.unknown_callers);
    assert_eq!(liveness.live_in(a(F)), Some(FlagSet::EMPTY));
    assert_eq!(liveness.live_out(a(F + 2)), Some(set_of(&[Flag::Z])));
    let stats = liveness.stats();
    // moveq sets N Z V C and addq X N Z V C: all but addq's Z go. The caller's moveq #1,d2
    // sets N Z V C, none of them live; move to CCR is not a SetFlag.
    assert_eq!(stats.total_ops(), 4 + 5 + 4);
    assert_eq!(stats.total_removed(), 4 + 4 + 4);
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_harness_dropping_every_flag_is_caught() {
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[MOVEQ_D0, ADDQ_D0, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());
    let mut harness = Harness::with_prune(&analysis, &liveness, &image.bytes, |lifted| Lifted {
        body: flags::prune(&lifted.body, FlagSet::EMPTY, |_| FlagSet::EMPTY),
        ..lifted.clone()
    });

    let error = harness
        .run(a(F), 0, CAP)
        .expect_err("the caller's Z is lost");

    assert!(error.contains("[Z]"), "{error}");
}

#[test]
fn test_liveness_flag_passes_through_a_callee_that_leaves_it() {
    // reset: cmp.w d1,d0; bsr g; bcs skip; moveq #1,d2; skip: move.w d3,ccr; rts
    let mut reset = vec![CMP_D1_D0];
    reset.extend(bsr(RESET + 2, G));
    reset.extend([BCS_SKIP, MOVEQ_D2, MOVE_D3_CCR, RTS]);
    let image = Image::new().words(RESET, &reset).words(G, &[NOP, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    let summary = liveness.summary(a(G)).expect("g is a function");
    assert_eq!(summary.defines, FlagSet::EMPTY);
    assert_eq!(summary.return_live, set_of(&[Flag::C]));
    assert_eq!(liveness.live_in(a(RESET + 2)), Some(set_of(&[Flag::C])));
    let pruned = liveness.prune(liveness.lifted(a(RESET)).expect("cmp is analysed"));
    let kept: FlagSet = pruned
        .body
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::SetFlag { flag, .. } => Some(*flag),
            _ => None,
        })
        .collect();
    assert_eq!(kept, set_of(&[Flag::C]));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_recursive_function_reaches_a_fixpoint() {
    // h: subq.w #1,d0; beq done; bsr h; done: rts
    let mut h = vec![SUBQ_D0, BEQ_SKIP + 2];
    h.extend(bsr(H + 4, H));
    h.push(RTS);
    let image = Image::new().words(RESET, &caller_reading_z(H)).words(H, &h);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    let summary = liveness.summary(a(H)).expect("h is a function");
    assert_eq!(summary.reads, FlagSet::EMPTY);
    assert_eq!(summary.defines, FlagSet::ALL);
    assert_eq!(summary.return_live, set_of(&[Flag::Z]));
    assert_eq!(liveness.live_out(a(H)), Some(set_of(&[Flag::Z])));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_move_from_sr_keeps_every_flag() {
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[MOVEQ_D0, ADDQ_D0, MOVE_SR_D2, MOVE_D3_CCR, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.live_out(a(F)), Some(FlagSet::EMPTY));
    assert_eq!(liveness.live_out(a(F + 2)), Some(FlagSet::ALL));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_unresolved_jump_keeps_every_flag_and_every_return() {
    // reset: bsr f; addq.w #1,d0; jmp (a0)
    let mut reset = bsr(RESET, F).to_vec();
    reset.extend([ADDQ_D0, 0x4ED0]);
    let image = Image::new().words(RESET, &reset).words(F, &[ADDQ_D0, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.live_out(a(RESET + 4)), Some(FlagSet::ALL));
    let summary = liveness.summary(a(F)).expect("f is a function");
    assert!(summary.unknown_callers);
    assert_eq!(summary.return_live, FlagSet::ALL);
    let stops = assert_equivalent(&image, &analysis, &liveness);
    assert!(stops.contains(&Stop::OffTable));
}

#[test]
fn test_liveness_rtr_overwrites_every_flag_and_rte_checks_privilege() {
    // A handler that sets flags before RTE, and f: addq.w #1,d0; move.w d1,-(sp); rtr
    let image = Image::new()
        .vector(32, G)
        .words(G, &[ADDQ_D0, RTE])
        .words(RESET, &caller_reading_z(F))
        .words(F, &[ADDQ_D0, 0x3F01, RTR]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    // RTE is privileged: in user mode it takes the privilege violation exception, which
    // pushes the status register, so every flag is live before it.
    assert_eq!(liveness.live_out(a(G)), Some(FlagSet::ALL));
    assert_eq!(liveness.live_out(a(F)), Some(FlagSet::EMPTY));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_popped_caller_keeps_the_grandcaller_flags() {
    // reset: tst.w d0; bsr f; beq skip; moveq #1,d2; skip: move.w d3,ccr; rts
    // f: bsr g; addq.w #1,d0; rts      g: addq.l #4,sp; rts (returns past f)
    let mut reset = vec![TST_D0];
    reset.extend(bsr(RESET + 2, F));
    reset.extend([BEQ_SKIP, MOVEQ_D2, MOVE_D3_CCR, RTS]);
    let mut f = bsr(F, G).to_vec();
    f.extend([ADDQ_D0, RTS]);
    let image = Image::new()
        .words(RESET, &reset)
        .words(F, &f)
        .words(G, &[0x588F, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.unbalanced_exits().collect::<Vec<_>>(), [a(G + 2)]);
    let live = liveness.live_out(a(RESET)).expect("tst is analysed");
    assert!(live.contains(Flag::Z));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_popped_caller_tail_jump_keeps_the_grandcaller_flags() {
    // reset: bsr h; tst.w d0; bsr f; beq skip; moveq #1,d2; skip: move.w d3,ccr; rts
    // f: bsr g; addq.w #1,d0; rts      g: addq.l #4,sp; bra h (h's rts returns past f)
    let mut reset = bsr(RESET, H).to_vec();
    reset.push(TST_D0);
    reset.extend(bsr(RESET + 6, F));
    reset.extend([BEQ_SKIP, MOVEQ_D2, MOVE_D3_CCR, RTS]);
    let mut f = bsr(F, G).to_vec();
    f.extend([ADDQ_D0, RTS]);
    let mut g = vec![0x588F];
    g.extend(bra(G + 2, H));
    let image = Image::new()
        .words(RESET, &reset)
        .words(F, &f)
        .words(G, &g)
        .words(H, &[RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.unbalanced_exits().collect::<Vec<_>>(), [a(G + 2)]);
    let live = liveness.live_out(a(RESET + 4)).expect("tst is analysed");
    assert!(live.contains(Flag::Z));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_popped_caller_falling_into_function_keeps_the_grandcaller_flags() {
    // As above, but g: addq.l #4,sp falls into h, which reset also calls.
    let mut reset = bsr(RESET, G + 2).to_vec();
    reset.push(TST_D0);
    reset.extend(bsr(RESET + 6, F));
    reset.extend([BEQ_SKIP, MOVEQ_D2, MOVE_D3_CCR, RTS]);
    let mut f = bsr(F, G).to_vec();
    f.extend([ADDQ_D0, RTS]);
    let image = Image::new()
        .words(RESET, &reset)
        .words(F, &f)
        .words(G, &[0x588F, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.unbalanced_exits().collect::<Vec<_>>(), [a(G)]);
    let live = liveness.live_out(a(RESET + 4)).expect("tst is analysed");
    assert!(live.contains(Flag::Z));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_exceptions_read_the_whole_status_register() {
    // f: addq.w #1,d2; chk.w d1,d0; addq.w #1,d2; divu.w d1,d0; rts
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[ADDQ_D2, 0x4181, ADDQ_D2, 0x80C1, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    // CHK sets N Z V C before its trap, so only X stays live from before it; DIVU traps on
    // a zero divisor before writing any flag.
    assert_eq!(liveness.live_out(a(F)), Some(set_of(&[Flag::X])));
    assert_eq!(liveness.live_out(a(F + 4)), Some(FlagSet::ALL));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_tail_jump_target_inherits_return_needs() {
    // reset: bsr f; beq skip; moveq #1,d2; skip: bsr g; move.w d3,ccr; rts
    // f: addq.w #1,d0; bra g      g: tst.w d0; rts
    let mut reset = bsr(RESET, F).to_vec();
    reset.extend([BEQ_SKIP, MOVEQ_D2]);
    reset.extend(bsr(RESET + 8, G));
    reset.extend([MOVE_D3_CCR, RTS]);
    let mut f = vec![ADDQ_D0];
    f.extend(bra(F + 2, G));
    let image = Image::new()
        .words(RESET, &reset)
        .words(F, &f)
        .words(G, &[TST_D0, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    let g = liveness.summary(a(G)).expect("g is a function");
    assert!(g.return_live.contains(Flag::Z));
    assert_eq!(liveness.live_out(a(F)), Some(FlagSet::EMPTY));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_addx_chain_keeps_x_and_z() {
    // f: add.l d0,d2; addx.l d1,d3; move.w d3,ccr; rts
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[0xD480, 0xD781, MOVE_D3_CCR, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.live_out(a(F)), Some(set_of(&[Flag::X, Flag::Z])));
    assert_equivalent(&image, &analysis, &liveness);
}

#[test]
fn test_liveness_check_points_keep_every_flag_at_block_starts() {
    // reset: bsr f; ...      f: addq.w #1,d0; bne skip; addq.w #1,d0; skip: moveq #0,d0; rts
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[ADDQ_D0, 0x6602, ADDQ_D0, MOVEQ_D0, RTS]);
    let analysis = image.analyze();
    let checked = FlagLiveness::new(
        &analysis,
        LivenessOptions {
            check_points_read_sr: true,
        },
    );
    let plain = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(checked.live_out(a(F + 4)), Some(FlagSet::ALL));
    assert_eq!(plain.live_out(a(F + 4)), Some(FlagSet::EMPTY));
    assert!(checked.stats().total_removed() < plain.stats().total_removed());
    assert_equivalent(&image, &analysis, &checked);
    assert_equivalent(&image, &analysis, &plain);
}

#[test]
fn test_liveness_counted_loop_reaches_a_fixpoint() {
    // f: moveq #3,d0; loop: subq.w #1,d0; bne loop; move.w d3,ccr; rts
    let image = Image::new()
        .words(RESET, &caller_reading_z(F))
        .words(F, &[0x7003, SUBQ_D0, 0x66FC, MOVE_D3_CCR, RTS]);
    let analysis = image.analyze();
    let liveness = FlagLiveness::new(&analysis, LivenessOptions::default());

    assert_eq!(liveness.live_out(a(F)), Some(FlagSet::EMPTY));
    assert_eq!(liveness.live_out(a(F + 2)), Some(set_of(&[Flag::Z])));
    let mut harness = Harness::new(&analysis, &liveness, &image.bytes);
    let run = harness.run(a(F), 1, CAP).expect("equivalent");
    assert_eq!((run.stop, run.steps), (Stop::Returned, 1 + 3 * 2 + 2));
    assert!(run.executed.total_removed() > 0);
    assert_equivalent(&image, &analysis, &liveness);
}
