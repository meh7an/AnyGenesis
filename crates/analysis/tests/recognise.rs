//! The jump-table recogniser over synthetic images, one per dispatch idiom, hand-assembled
//! word by word. Every image is analysed from its vectors alone, as game builds do, and
//! the last ones also with a profile of where a run's computed jumps went.

use anygenesis_analysis::{
    Analysis, DiscoveryStage, EntryKind, ResolvedTable, TargetKind, analyze, analyze_profiled,
    analyze_stage,
};
use anygenesis_rom::Rom;
use anygenesis_shared::{Address, Profile};

const ROM_SIZE: usize = 0x1000;
/// Every handler vector points at an `rte` here.
const HANDLER: u32 = 0x200;
const RESET: u32 = 0x300;

const RTS: u16 = 0x4E75;
const RTE: u16 = 0x4E73;

/// A ROM image under construction.
struct Image {
    bytes: Vec<u8>,
}

impl Image {
    fn new() -> Self {
        let mut image = Self {
            bytes: vec![0; ROM_SIZE],
        };
        for index in 2..48 {
            image = image.long(index * 4, HANDLER);
        }
        image.long(4, RESET).words(HANDLER, &[RTE])
    }

    fn long(mut self, address: u32, value: u32) -> Self {
        let offset = address as usize;
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
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

    fn analyze(&self) -> Analysis {
        analyze(&self.rom())
    }
}

fn a(raw: u32) -> Address {
    Address::new(raw)
}

fn addresses(raw: &[u32]) -> Vec<Address> {
    raw.iter().copied().map(a).collect()
}

/// The only table the analysis used, after checking every site was resolved.
fn only_table(analysis: &Analysis) -> &ResolvedTable {
    assert!(analysis.unresolved.is_empty(), "{:?}", analysis.unresolved);
    assert_eq!(analysis.jump_tables.len(), 1, "{:?}", analysis.jump_tables);
    &analysis.jump_tables[0]
}

/// `move.w tbl(pc,d0.w),d1 / jmp tbl(pc,d1.w)` at 0x300, two word offsets at 0x308, and
/// the two `rts` they lead to.
fn word_table_image() -> Image {
    Image::new().words(
        RESET,
        &[0x323B, 0x0006, 0x4EFB, 0x1002, 0x0004, 0x0006, RTS, RTS],
    )
}

#[test]
fn test_recognise_pc_word_offset_table_resolves_site() {
    let analysis = word_table_image().analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x308));
    assert_eq!(table.kind, EntryKind::WordOffset);
    assert_eq!(table.count, 2);
    assert_eq!(table.site, Some(a(0x304)));
    assert_eq!(table.target, TargetKind::Block);
    assert_eq!(table.targets, addresses(&[0x30C, 0x30E]));
}

#[test]
fn test_recognise_vectors_stage_leaves_site_unresolved() {
    let analysis = analyze_stage(&word_table_image().rom(), DiscoveryStage::Vectors);
    assert!(analysis.jump_tables.is_empty());
    assert_eq!(analysis.unresolved.len(), 1);
}

#[test]
fn test_recognise_andi_mask_caps_heuristic_count() {
    // andi.w #2,d0 allows byte offsets 0 and 2: two entries of three that look like code.
    let image = Image::new().words(
        RESET,
        &[
            0x0240, 0x0002, 0x323B, 0x0006, 0x4EFB, 0x1002, 0x0006, 0x0008, 0x000A, RTS, RTS, RTS,
        ],
    );
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x30C));
    assert_eq!(table.count, 2);
    assert!(!analysis.instructions.contains_key(&a(0x316)));
}

#[test]
fn test_recognise_cmp_bound_scaled_by_add_caps_count() {
    // cmpi.w #1,d0 / bhi.s: index 0 or 1, doubled by add.w d0,d0 into byte offsets 0 and 2.
    let image = Image::new().words(
        RESET,
        &[
            0x0C40, 0x0001, 0x6214, 0xD040, 0x323B, 0x0006, 0x4EFB, 0x1002, 0x0006, 0x0008, 0x000A,
            RTS, RTS, RTS,
        ],
    );
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x310));
    assert_eq!(table.count, 2);
    assert_eq!(table.targets, addresses(&[0x316, 0x318]));
}

#[test]
fn test_recognise_bra_w_row_counts_rows_up_to_first_target() {
    // jmp tbl(pc,d0.w) at 0x300 onto two bra.w rows leading to 0x30C and 0x30E.
    let image = Image::new().words(
        RESET,
        &[0x4EFB, 0x0002, 0x6000, 0x0006, 0x6000, 0x0004, RTS, RTS],
    );
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x304));
    assert_eq!(table.kind, EntryKind::BraW);
    assert_eq!(table.count, 2);
    assert!(analysis.instructions.contains_key(&a(0x30E)));
}

#[test]
fn test_recognise_pc_long_table_skips_leading_non_pointer() {
    // movea.l tbl-4(pc,d0.w),a0 / jsr (a0) / rts; the long at tbl-4 is no code pointer,
    // two function pointers follow, then a long outside the ROM.
    let image = Image::new()
        .words(RESET, &[0x207B, 0x0006, 0x4E90, RTS])
        .long(0x30C, 0x320)
        .long(0x310, 0x322)
        .long(0x314, 0xFFFF_FFFF)
        .words(0x320, &[RTS, RTS]);
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x30C));
    assert_eq!(table.kind, EntryKind::Long);
    assert_eq!(table.count, 2);
    assert_eq!(table.target, TargetKind::Function);
    assert!(analysis.functions.contains_key(&a(0x320)));
    assert!(analysis.functions.contains_key(&a(0x322)));
}

#[test]
fn test_recognise_lea_then_movea_long_table_resolves_jump() {
    // lea tbl(pc),a1 / movea.l (a1,d0.w),a0 / jmp (a0), two pointers at 0x30C.
    let image = Image::new()
        .words(RESET, &[0x43FA, 0x000A, 0x2071, 0x0000, 0x4ED0])
        .long(0x30C, 0x320)
        .long(0x310, 0x322)
        .long(0x314, 0xFFFF_FFFF)
        .words(0x320, &[RTS, RTS]);
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x30C));
    assert_eq!(table.kind, EntryKind::Long);
    assert_eq!(table.site, Some(a(0x308)));
    assert_eq!(table.targets, addresses(&[0x320, 0x322]));
}

#[test]
fn test_recognise_lea_word_offset_register_idiom_calls_entries() {
    // lea tbl(pc),a0 / move.w (a0,d0.w),d0 / lea (a0,d0.w),a0 / jsr (a0) / rts, two offsets
    // at 0x310.
    let image = Image::new().words(
        RESET,
        &[
            0x41FA, 0x000E, 0x3030, 0x0000, 0x41F0, 0x0000, 0x4E90, RTS, 0x0004, 0x0006, RTS, RTS,
        ],
    );
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x310));
    assert_eq!(table.kind, EntryKind::WordOffset);
    assert_eq!(table.count, 2);
    assert_eq!(table.target, TargetKind::Function);
    assert!(analysis.functions.contains_key(&a(0x314)));
    assert!(analysis.functions.contains_key(&a(0x316)));
}

#[test]
fn test_recognise_table_ends_at_address_code_names() {
    // lea data(pc),a1 names 0x310, the word after two offsets; that word would also lead to
    // an rts, but it starts the next block of data.
    let image = Image::new()
        .words(
            RESET,
            &[
                0x43FA, 0x000E, 0x323B, 0x0006, 0x4EFB, 0x1002, 0x0014, 0x0016, 0x0018,
            ],
        )
        .words(0x320, &[RTS, RTS, RTS]);
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x30C));
    assert_eq!(table.count, 2);
    assert!(!analysis.instructions.contains_key(&a(0x324)));
}

#[test]
fn test_recognise_offset_into_table_ends_it() {
    // The third word leads to itself, where `ori.b` would decode, so it is data.
    let image = Image::new()
        .words(
            RESET,
            &[
                0x323B, 0x0006, 0x4EFB, 0x1002, 0x000A, 0x000C, 0x0004, 0x0000, 0x0000,
            ],
        )
        .words(0x312, &[RTS, RTS]);
    let analysis = image.analyze();
    let table = only_table(&analysis);
    assert_eq!(table.address, a(0x308));
    assert_eq!(table.count, 2);
    assert!(!analysis.instructions.contains_key(&a(0x30C)));
}

#[test]
fn test_recognise_table_behind_table_found_in_later_round() {
    // The first table's only entry leads to a second dispatch with its own table.
    let image = Image::new()
        .words(RESET, &[0x323B, 0x0006, 0x4EFB, 0x1002, 0x0002])
        .words(0x30A, &[0x323B, 0x0006, 0x4EFB, 0x1002, 0x0002, RTS]);
    let analysis = image.analyze();
    assert!(analysis.unresolved.is_empty(), "{:?}", analysis.unresolved);
    let found: Vec<(Address, u32)> = analysis
        .jump_tables
        .iter()
        .map(|table| (table.address, table.count))
        .collect();
    assert_eq!(found, [(a(0x308), 1), (a(0x312), 1)]);
    assert!(analysis.instructions.contains_key(&a(0x314)));
}

#[test]
fn test_recognise_register_jump_without_idiom_stays_unresolved() {
    // movea.l (a1),a0 / jmp (a0): nothing says where a1 points.
    let image = Image::new().words(RESET, &[0x2051, 0x4ED0]);
    let analysis = image.analyze();
    assert!(analysis.jump_tables.is_empty());
    assert_eq!(analysis.unresolved.len(), 1);
}

#[test]
fn test_recognise_odd_first_target_declines_table() {
    let image = Image::new().words(RESET, &[0x323B, 0x0006, 0x4EFB, 0x1002, 0x0003, RTS]);
    let analysis = image.analyze();
    assert!(analysis.jump_tables.is_empty());
    assert_eq!(analysis.unresolved.len(), 1);
}

/// `jsr (a0)` then `rts` at 0x300, and an `rts` at 0x500 that nothing in the ROM names.
fn register_call_image() -> Image {
    Image::new()
        .words(RESET, &[0x4E90, RTS])
        .words(0x500, &[RTS])
}

#[test]
fn test_profile_targets_resolve_a_register_call_no_table_explains() {
    let image = register_call_image();
    let alone = image.analyze();
    assert_eq!(alone.unresolved.len(), 1);
    assert!(!alone.functions.contains_key(&a(0x500)));

    let mut profile = Profile::new(ROM_SIZE as u32);
    profile.computed(RESET, 0x500);
    let analysis = analyze_profiled(&image.rom(), &profile);
    assert!(analysis.unresolved.is_empty(), "{:?}", analysis.unresolved);
    assert!(analysis.functions.contains_key(&a(0x500)));
}

#[test]
fn test_profile_target_a_table_leaves_out_becomes_a_function() {
    let image = word_table_image().words(0x320, &[RTS]);
    let mut profile = Profile::new(ROM_SIZE as u32);
    profile.computed(0x304, 0x30C);
    profile.computed(0x304, 0x320);
    let analysis = analyze_profiled(&image.rom(), &profile);
    assert_eq!(only_table(&analysis).count, 2);
    assert!(analysis.functions.contains_key(&a(0x320)));
    assert!(!image.analyze().functions.contains_key(&a(0x320)));
}
