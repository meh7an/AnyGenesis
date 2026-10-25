//! Control-flow discovery over synthetic images, hand-assembled word by word.

use std::collections::BTreeSet;

use anygenesis_analysis::{
    Analysis, AnalysisError, Call, CallKind, Diagnostic, Edge, EdgeKind, Origin, Source,
    Suggestion, TailJump, Terminator, UnresolvedSite, analyze,
};
use anygenesis_gamecfg::{EntryKind, GameConfig};
use anygenesis_m68k::DecodeError;
use anygenesis_rom::Rom;
use anygenesis_shared::Address;

const ROM_SIZE: usize = 0x1000;
/// Every handler vector points at an `rte` here.
const HANDLER: u32 = 0x200;
const RESET: u32 = 0x300;

const RTS: u16 = 0x4E75;
const RTE: u16 = 0x4E73;
const NOP: u16 = 0x4E71;

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

    fn rom(&self) -> Rom {
        Rom::from_bytes(self.bytes.clone()).expect("synthetic image loads")
    }

    /// Analyses the image with a config made of its SHA-1 and `hints`.
    fn analyze(&self, hints: &str) -> Analysis {
        let rom = self.rom();
        let text = format!("sha1 = \"{}\"\n{hints}", rom.sha1());
        let config = GameConfig::parse(&text).expect("valid config");
        analyze(&rom, &config).expect("SHA-1 matches")
    }
}

fn a(raw: u32) -> Address {
    Address::new(raw)
}

fn edge(target: u32, kind: EdgeKind) -> Edge {
    Edge {
        target: a(target),
        kind,
    }
}

fn set(addresses: &[u32]) -> BTreeSet<Address> {
    addresses.iter().copied().map(a).collect()
}

#[test]
fn test_analyze_straight_function_gives_one_returning_block() {
    let analysis = Image::new().words(RESET, &[0x7000, RTS]).analyze("");
    let function = &analysis.functions[&a(RESET)];
    assert_eq!(function.origins, [Origin::Reset]);
    assert_eq!(function.blocks, set(&[RESET]));
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.end, a(0x304));
    assert_eq!(block.last, a(0x302));
    assert_eq!(block.terminator, Terminator::Return);
    assert!(block.successors.is_empty());
    assert_eq!(
        analysis.blocks[&a(HANDLER)].terminator,
        Terminator::ReturnFromException
    );
}

#[test]
fn test_analyze_dbcc_loop_splits_blocks_at_loop_target() {
    // moveq #3,d0 / loop: nop / dbf d0,loop / rts
    let analysis = Image::new()
        .words(RESET, &[0x7003, NOP, 0x51C8, 0xFFFC, RTS])
        .analyze("");
    let entry = &analysis.blocks[&a(0x300)];
    assert_eq!(entry.terminator, Terminator::Fallthrough);
    assert_eq!(entry.successors, [edge(0x302, EdgeKind::Fallthrough)]);
    let body = &analysis.blocks[&a(0x302)];
    assert_eq!(body.end, a(0x308));
    assert_eq!(body.terminator, Terminator::DecrementBranch);
    assert_eq!(
        body.successors,
        [
            edge(0x302, EdgeKind::Taken),
            edge(0x308, EdgeKind::Fallthrough)
        ]
    );
    assert_eq!(analysis.blocks[&a(0x308)].terminator, Terminator::Return);
    assert_eq!(
        analysis.functions[&a(RESET)].blocks,
        set(&[0x300, 0x302, 0x308])
    );
}

#[test]
fn test_analyze_conditional_branch_lists_taken_then_fallthrough() {
    // beq.s skip / moveq #1,d0 / rts / skip: rts
    let analysis = Image::new()
        .words(RESET, &[0x6704, 0x7001, RTS, RTS])
        .analyze("");
    let block = &analysis.blocks[&a(0x300)];
    assert_eq!(block.terminator, Terminator::Conditional);
    assert_eq!(
        block.successors,
        [
            edge(0x306, EdgeKind::Taken),
            edge(0x302, EdgeKind::Fallthrough)
        ]
    );
    assert_eq!(analysis.blocks[&a(0x302)].end, a(0x306));
    assert_eq!(analysis.blocks[&a(0x306)].terminator, Terminator::Return);
}

#[test]
fn test_analyze_bsr_targets_become_functions_and_tail_jump_recorded() {
    // bsr.w f1 / bsr.w f2 / rts; f1: moveq #1,d0 / bra.s f2; f2: rts
    let analysis = Image::new()
        .words(RESET, &[0x6100, 0x000E, 0x6100, 0x001A, RTS])
        .words(0x310, &[0x7001, 0x600C])
        .words(0x320, &[RTS])
        .analyze("");
    let caller = &analysis.blocks[&a(RESET)];
    assert_eq!(caller.end, a(0x30A));
    assert_eq!(
        caller.calls,
        [
            Call {
                site: a(0x300),
                kind: CallKind::Bsr,
                targets: vec![a(0x310)],
            },
            Call {
                site: a(0x304),
                kind: CallKind::Bsr,
                targets: vec![a(0x320)],
            },
        ]
    );
    let f1 = &analysis.functions[&a(0x310)];
    assert_eq!(f1.callers, set(&[0x300]));
    assert_eq!(f1.blocks, set(&[0x310]));
    assert_eq!(
        f1.tail_jumps,
        [TailJump {
            site: a(0x312),
            target: a(0x320),
        }]
    );
    assert_eq!(analysis.blocks[&a(0x320)].functions, set(&[0x320]));
    assert!(analysis.is_tail_jump(a(0x310), a(0x320)));
    assert!(!analysis.is_tail_jump(a(0x320), a(0x320)));
    assert_eq!(analysis.shared_blocks().count(), 0);
}

#[test]
fn test_analyze_block_reached_by_two_functions_is_shared() {
    // bsr.w f1 / bsr.w f2 / rts; f1 and f2 load d0, then bra.w common; common: rts
    let analysis = Image::new()
        .words(RESET, &[0x6100, 0x000E, 0x6100, 0x001A, RTS])
        .words(0x310, &[0x7001, 0x6000, 0x001C])
        .words(0x320, &[0x7002, 0x6000, 0x000C])
        .words(0x330, &[RTS])
        .analyze("");
    let shared: Vec<Address> = analysis.shared_blocks().map(|block| block.start).collect();
    assert_eq!(shared, [a(0x330)]);
    assert_eq!(analysis.blocks[&a(0x330)].functions, set(&[0x310, 0x320]));
    assert!(analysis.functions[&a(0x310)].tail_jumps.is_empty());
}

/// `move.w tbl(pc,d0.w),d1 / jmp tbl(pc,d1.w)` at 0x300, two word offsets at 0x308, and
/// the two `rts` they lead to at 0x30C and 0x30E.
fn word_table_image() -> Image {
    Image::new().words(
        RESET,
        &[0x323B, 0x0006, 0x4EFB, 0x1002, 0x0004, 0x0006, RTS, RTS],
    )
}

#[test]
fn test_analyze_hinted_word_offset_table_resolves_entries() {
    let analysis = word_table_image()
        .analyze("[[jump_table]]\naddress = 0x308\ncount = 2\nkind = \"word_offset\"\n");
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.terminator, Terminator::IndirectJump);
    let table = EdgeKind::Table { table: a(0x308) };
    assert_eq!(block.successors, [edge(0x30C, table), edge(0x30E, table)]);
    assert_eq!(
        analysis.functions[&a(RESET)].blocks,
        set(&[0x300, 0x30C, 0x30E])
    );
    assert_eq!(analysis.jump_tables.len(), 1);
    assert_eq!(analysis.jump_tables[0].site, Some(a(0x304)));
    assert_eq!(analysis.jump_tables[0].targets, [a(0x30C), a(0x30E)]);
    assert!(analysis.unresolved.is_empty());
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, Diagnostic::TableOverlapsCode { .. }))
    );
}

#[test]
fn test_analyze_unhinted_word_table_suggests_hint_with_count_guess() {
    let analysis = word_table_image().analyze("");
    assert_eq!(
        analysis.unresolved,
        [UnresolvedSite {
            site: a(0x304),
            is_call: false,
            suggestion: Suggestion::JumpTable {
                address: a(0x308),
                kind: EntryKind::WordOffset,
                count_guess: Some(2),
            },
        }]
    );
    let report = analysis.report().to_string();
    assert!(
        report.contains(
            "# [[jump_table]]\n# address = 0x000308\n# count = 2  # guessed, verify\n\
             # kind = \"word_offset\"\n# site = 0x000304\n"
        ),
        "{report}"
    );
    assert!(report.contains("0x000300  move.w"), "{report}");
    assert!(report.contains("0x000304  jmp"), "{report}");
}

/// `jmp tbl(pc,d0.w)` at 0x300, two `bra.w` entries at 0x304, and their targets at 0x30C
/// and 0x30E.
fn bra_table_image() -> Image {
    Image::new().words(
        RESET,
        &[0x4EFB, 0x0002, 0x6000, 0x0006, 0x6000, 0x0004, RTS, RTS],
    )
}

#[test]
fn test_analyze_bra_w_table_entries_are_code() {
    let analysis =
        bra_table_image().analyze("[[jump_table]]\naddress = 0x304\ncount = 2\nkind = \"bra_w\"\n");
    let table = EdgeKind::Table { table: a(0x304) };
    assert_eq!(
        analysis.blocks[&a(RESET)].successors,
        [edge(0x304, table), edge(0x308, table)]
    );
    assert_eq!(
        analysis.blocks[&a(0x304)].successors,
        [edge(0x30C, EdgeKind::Jump)]
    );
    assert_eq!(
        analysis.blocks[&a(0x308)].successors,
        [edge(0x30E, EdgeKind::Jump)]
    );
    assert_eq!(
        analysis.functions[&a(RESET)].blocks,
        set(&[0x300, 0x304, 0x308, 0x30C, 0x30E])
    );
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, Diagnostic::NotBraW { .. }))
    );
}

#[test]
fn test_analyze_unhinted_pc_index_jump_without_move_suggests_bra_w() {
    let analysis = bra_table_image().analyze("");
    assert_eq!(
        analysis.unresolved[0].suggestion,
        Suggestion::JumpTable {
            address: a(0x304),
            kind: EntryKind::BraW,
            count_guess: Some(2),
        }
    );
}

#[test]
fn test_analyze_long_table_at_jsr_site_makes_functions() {
    // jsr (a1) / rts, with a table of two pointers at 0x400.
    let analysis = Image::new()
        .words(RESET, &[0x4E91, RTS])
        .words(0x400, &[0x0000, 0x0410, 0x0000, 0x0420])
        .words(0x410, &[RTS])
        .words(0x420, &[RTS])
        .analyze("[[jump_table]]\naddress = 0x400\ncount = 2\nkind = \"long\"\nsite = 0x300\n");
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.terminator, Terminator::Return);
    assert_eq!(
        block.calls,
        [Call {
            site: a(0x300),
            kind: CallKind::Jsr,
            targets: vec![a(0x410), a(0x420)],
        }]
    );
    for entry in [0x410, 0x420] {
        let function = &analysis.functions[&a(entry)];
        assert_eq!(function.origins, [Origin::Table { table: a(0x400) }]);
        assert_eq!(function.callers, set(&[0x300]));
    }
}

#[test]
fn test_analyze_register_jumps_reported_unresolved() {
    // jsr (a0) / jmp (a0)
    let analysis = Image::new().words(RESET, &[0x4E90, 0x4ED0]).analyze("");
    assert_eq!(
        analysis.unresolved,
        [
            UnresolvedSite {
                site: a(0x300),
                is_call: true,
                suggestion: Suggestion::Indirect,
            },
            UnresolvedSite {
                site: a(0x302),
                is_call: false,
                suggestion: Suggestion::Indirect,
            },
        ]
    );
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.terminator, Terminator::IndirectJump);
    assert!(block.successors.is_empty());
    assert_eq!(
        block.calls,
        [Call {
            site: a(0x300),
            kind: CallKind::Jsr,
            targets: Vec::new(),
        }]
    );
    let report = analysis.report().to_string();
    assert!(
        report.contains("| Unresolved indirect sites | 2 |"),
        "{report}"
    );
    assert!(report.contains("### 0x000302 (jump)"), "{report}");
    assert!(
        report.contains("# [[indirect]]\n# site = 0x000302\n# targets = []\n"),
        "{report}"
    );
}

#[test]
fn test_report_context_stops_after_instruction_without_fallthrough() {
    // rts; a hinted function right after it: jmp (a0)
    let analysis = Image::new()
        .words(RESET, &[RTS, 0x4ED0])
        .analyze("[[function]]\naddress = 0x302\n");
    let report = analysis.report().to_string();
    assert!(report.contains("```text\n0x000302  jmp"), "{report}");
    assert!(!report.contains("0x000300  rts"), "{report}");
}

#[test]
fn test_analyze_indirect_hint_supplies_jump_targets() {
    let analysis = Image::new()
        .words(RESET, &[0x4ED0])
        .words(0x310, &[RTS])
        .words(0x320, &[RTS])
        .analyze("[[indirect]]\nsite = 0x300\ntargets = [0x310, 0x320]\n");
    assert!(analysis.unresolved.is_empty());
    assert_eq!(
        analysis.blocks[&a(RESET)].successors,
        [edge(0x310, EdgeKind::Hinted), edge(0x320, EdgeKind::Hinted)]
    );
    assert_eq!(
        analysis.functions[&a(RESET)].blocks,
        set(&[0x300, 0x310, 0x320])
    );
}

#[test]
fn test_analyze_branch_into_instruction_middle_flags_overlap() {
    // beq.s into the immediate of move.l #$4E754E75,d0, whose words decode as rts.
    let analysis = Image::new()
        .words(RESET, &[0x6702, 0x203C, 0x4E75, 0x4E75, RTS])
        .analyze("");
    assert!(analysis.diagnostics.contains(&Diagnostic::Overlap {
        first: a(0x302),
        second: a(0x304),
    }));
    assert_eq!(analysis.blocks[&a(0x302)].end, a(0x30A));
    assert_eq!(analysis.blocks[&a(0x304)].terminator, Terminator::Return);
}

#[test]
fn test_analyze_bad_targets_reported_and_not_decoded() {
    // jsr ($8000).w (RAM) / jsr $100000 (past the ROM) / jmp $301 (odd)
    let analysis = Image::new()
        .words(
            RESET,
            &[
                0x4EB8, 0x8000, 0x4EB9, 0x0010, 0x0000, 0x4EF9, 0x0000, 0x0301,
            ],
        )
        .analyze("");
    for diagnostic in [
        Diagnostic::RamTarget {
            source: Source::Instruction(a(0x300)),
            target: a(0xFF_8000),
        },
        Diagnostic::OutOfRomTarget {
            source: Source::Instruction(a(0x304)),
            target: a(0x10_0000),
        },
        Diagnostic::OddTarget {
            source: Source::Instruction(a(0x30A)),
            target: a(0x301),
        },
    ] {
        assert!(analysis.diagnostics.contains(&diagnostic), "{diagnostic:?}");
    }
    assert_eq!(
        analysis.functions.keys().copied().collect::<Vec<_>>(),
        [a(HANDLER), a(RESET)]
    );
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.terminator, Terminator::Jump);
    assert_eq!(block.successors, [edge(0x301, EdgeKind::Jump)]);
    assert_eq!(block.calls[0].targets, [a(0xFF_8000)]);
}

#[test]
fn test_analyze_noreturn_hint_stops_decoding_after_call() {
    // bsr.w forever / bytes that are no instruction; forever: bra.s forever
    let analysis = Image::new()
        .words(RESET, &[0x6100, 0x000E, 0x4E7A])
        .words(0x310, &[0x60FE])
        .analyze("[[function]]\naddress = 0x310\nnoreturn = true\n");
    assert!(!analysis.instructions.contains_key(&a(0x304)));
    assert_eq!(
        analysis.blocks[&a(RESET)].terminator,
        Terminator::NoReturnCall
    );
    let forever = &analysis.functions[&a(0x310)];
    assert!(forever.noreturn);
    assert_eq!(forever.origins, [Origin::Hint { line: 3 }]);
    assert!(forever.tail_jumps.is_empty());
    assert_eq!(
        analysis.blocks[&a(0x310)].successors,
        [edge(0x310, EdgeKind::Jump)]
    );
    assert!(
        !analysis
            .diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, Diagnostic::DecodeFailed { .. }))
    );
}

#[test]
fn test_analyze_trap_calls_handler_and_falls_through() {
    // trap #0 / rts
    let analysis = Image::new().words(RESET, &[0x4E40, RTS]).analyze("");
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.terminator, Terminator::Trap);
    assert_eq!(block.successors, [edge(0x302, EdgeKind::Fallthrough)]);
    assert_eq!(
        block.calls,
        [Call {
            site: a(0x300),
            kind: CallKind::Trap,
            targets: vec![a(HANDLER)],
        }]
    );
    assert_eq!(analysis.blocks[&a(0x302)].terminator, Terminator::Return);
}

#[test]
fn test_analyze_stop_illegal_and_rtr_terminators() {
    // stop #$2700 / illegal; a hinted function: rtr
    let analysis = Image::new()
        .words(RESET, &[0x4E72, 0x2700, 0x4AFC])
        .words(0x310, &[0x4E77])
        .analyze("[[function]]\naddress = 0x310\n");
    let stop = &analysis.blocks[&a(RESET)];
    assert_eq!(stop.terminator, Terminator::Stop);
    assert_eq!(stop.successors, [edge(0x304, EdgeKind::Fallthrough)]);
    let illegal = &analysis.blocks[&a(0x304)];
    assert_eq!(illegal.terminator, Terminator::Exception);
    assert!(illegal.successors.is_empty());
    assert_eq!(
        analysis.blocks[&a(0x310)].terminator,
        Terminator::ReturnRestoringCcr
    );
}

#[test]
fn test_analyze_undecodable_bytes_end_block_invalid() {
    // moveq #0,d0 / 0x4E7A (MOVEC, not on the 68000)
    let analysis = Image::new().words(RESET, &[0x7000, 0x4E7A]).analyze("");
    assert!(analysis.diagnostics.contains(&Diagnostic::DecodeFailed {
        address: a(0x302),
        error: DecodeError::InvalidOpcode { opcode: 0x4E7A },
    }));
    let block = &analysis.blocks[&a(RESET)];
    assert_eq!(block.terminator, Terminator::Invalid);
    assert!(block.successors.is_empty());
}

#[test]
fn test_analyze_code_running_off_rom_reported() {
    let analysis = Image::new()
        .vector(1, 0xFFC)
        .words(0xFFC, &[NOP, NOP])
        .analyze("");
    assert!(
        analysis
            .diagnostics
            .contains(&Diagnostic::RunsOffRom { address: a(0xFFE) })
    );
    assert_eq!(analysis.blocks[&a(0xFFC)].terminator, Terminator::Invalid);
}

#[test]
fn test_analyze_data_regions_are_never_decoded() {
    // bra.s into the first region; a hinted nop runs into the second.
    let analysis = Image::new()
        .words(RESET, &[0x600E])
        .words(0x330, &[NOP])
        .analyze(
            "[[data]]\nstart = 0x310\nend = 0x320\n[[function]]\naddress = 0x330\n\
             [[data]]\nstart = 0x332\nend = 0x340\n",
        );
    assert!(analysis.diagnostics.contains(&Diagnostic::DataTarget {
        source: Source::Instruction(a(0x300)),
        target: a(0x310),
        region_line: 3,
    }));
    assert!(analysis.diagnostics.contains(&Diagnostic::FallsIntoData {
        address: a(0x330),
        region_line: 8,
    }));
    assert!(!analysis.instructions.contains_key(&a(0x310)));
    assert!(!analysis.instructions.contains_key(&a(0x332)));
}

#[test]
fn test_analyze_vectors_sharing_handler_reported_once() {
    let analysis = Image::new().words(RESET, &[RTS]).analyze("");
    let expected: Vec<u8> = (2..=11).chain([15]).chain(24..=47).collect();
    let shared: Vec<&Diagnostic> = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic, Diagnostic::SharedHandler { .. }))
        .collect();
    assert_eq!(
        shared,
        [&Diagnostic::SharedHandler {
            target: a(HANDLER),
            vectors: expected.clone(),
        }]
    );
    let origins: Vec<Origin> = expected.into_iter().map(Origin::Vector).collect();
    assert_eq!(analysis.functions[&a(HANDLER)].origins, origins);
}

#[test]
fn test_analyze_table_without_site_makes_its_targets_functions() {
    let analysis = Image::new()
        .words(RESET, &[RTS])
        .words(0x400, &[0x0000, 0x0410])
        .words(0x410, &[RTS])
        .analyze("[[jump_table]]\naddress = 0x400\ncount = 1\nkind = \"long\"\n");
    let function = &analysis.functions[&a(0x410)];
    assert_eq!(function.origins, [Origin::Table { table: a(0x400) }]);
    assert!(function.callers.is_empty());
    assert_eq!(analysis.jump_tables[0].site, None);
}

#[test]
fn test_analyze_block_table_without_site_leaves_unowned_blocks() {
    let analysis = Image::new()
        .words(RESET, &[RTS])
        .words(0x400, &[0x0000, 0x0410])
        .words(0x410, &[RTS])
        .analyze(
            "[[jump_table]]\naddress = 0x400\ncount = 1\nkind = \"long\"\ntarget = \"block\"\n",
        );
    assert!(!analysis.functions.contains_key(&a(0x410)));
    assert!(
        analysis
            .diagnostics
            .contains(&Diagnostic::UnownedBlock { start: a(0x410) })
    );
}

#[test]
fn test_analyze_table_site_that_is_not_indirect_reported() {
    let analysis = Image::new()
        .words(RESET, &[RTS])
        .words(0x400, &[0x0000, 0x0410])
        .words(0x410, &[RTS])
        .analyze("[[jump_table]]\naddress = 0x400\ncount = 1\nkind = \"long\"\nsite = 0x300\n");
    assert!(
        analysis
            .diagnostics
            .contains(&Diagnostic::TableSiteNotIndirect {
                table: a(0x400),
                site: a(0x300),
            })
    );
    assert!(analysis.functions.contains_key(&a(0x410)));
}

#[test]
fn test_analyze_config_for_other_rom_refused() {
    let rom = Image::new().words(RESET, &[RTS]).rom();
    let config =
        GameConfig::parse("sha1 = \"0000000000000000000000000000000000000000\"\n").expect("valid");
    assert!(matches!(
        analyze(&rom, &config),
        Err(AnalysisError::Sha1Mismatch { .. })
    ));
}
