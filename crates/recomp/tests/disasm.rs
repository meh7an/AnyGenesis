//! The AS-dialect listing over a synthetic image, hand-assembled word by word.

use anygenesis_analysis::{EntryKind, FunctionHint, Hints, JumpTable, TargetKind, analyze_with};
use anygenesis_recomp::disasm::write_listing;
use anygenesis_rom::Rom;
use anygenesis_shared::Address;

const ROM_SIZE: usize = 0x400;
const HANDLER: usize = 0x200;
const RESET: usize = 0x300;

/// Writes `words` at `address` in `bytes`.
fn put(bytes: &mut [u8], address: usize, words: &[u16]) {
    for (index, word) in words.iter().enumerate() {
        let offset = address + index * 2;
        bytes[offset..offset + 2].copy_from_slice(&word.to_be_bytes());
    }
}

/// Every vector points at an `rte`; reset calls a function and loops; a word-offset table
/// leads to two functions; nothing reaches the `nop`, `rts` at 0x340. Hinted functions at 0x350
/// (`move.b 0(a0),d0`) and 0x360 (`lea` into the middle of that move) cover AS's limits.
fn image() -> Vec<u8> {
    let mut bytes = vec![0; ROM_SIZE];
    for index in 2..48 {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&(HANDLER as u32).to_be_bytes());
    }
    bytes[4..8].copy_from_slice(&(RESET as u32).to_be_bytes());
    put(&mut bytes, HANDLER, &[0x4E73]);
    put(&mut bytes, RESET, &[0x6102, 0x60FE, 0x4E71, 0x4E75]);
    put(&mut bytes, 0x320, &[0x0010, 0x0012]);
    put(&mut bytes, 0x330, &[0x4E75, 0x4E75]);
    put(&mut bytes, 0x340, &[0x4E71, 0x4E75]);
    put(&mut bytes, 0x350, &[0x1028, 0x0000, 0x4E75]);
    put(&mut bytes, 0x360, &[0x41FA, 0xFFF0, 0x4E75]);
    bytes
}

fn listing(hints: &Hints) -> String {
    let rom = Rom::from_bytes(image()).expect("synthetic image loads");
    let analysis = analyze_with(&rom, hints);
    let mut out = Vec::new();
    write_listing(&mut out, rom.bytes(), &analysis).expect("writes to memory");
    String::from_utf8(out).expect("ASCII listing")
}

/// The word-offset table at 0x320, whose two entries are functions.
fn table_hints() -> Hints {
    Hints {
        jump_tables: vec![JumpTable {
            address: Address::new(0x320),
            count: 2,
            kind: EntryKind::WordOffset,
            site: None,
            target: Some(TargetKind::Function),
        }],
        ..Hints::default()
    }
}

/// The functions at 0x350 and 0x360, which cover AS's limits.
fn as_limit_hints() -> Hints {
    let function = |address| FunctionHint {
        address: Address::new(address),
        noreturn: false,
    };
    Hints {
        functions: vec![function(0x350), function(0x360)],
        ..Hints::default()
    }
}

/// The assembler text of each line, comments and trailing spaces removed.
fn code(listing: &str) -> Vec<&str> {
    listing
        .lines()
        .map(|line| line.split(';').next().unwrap_or_default().trim_end())
        .filter(|text| !text.is_empty())
        .collect()
}

#[test]
fn test_listing_prints_code_with_labels_and_branch_sizes() {
    let text = listing(&table_hints());
    let lines = code(&text);
    let start = lines
        .iter()
        .position(|line| *line == "fn_000300:")
        .expect("reset entry is labelled");
    assert_eq!(
        lines[start..start + 7],
        [
            "fn_000300:",
            "\tbsr.s\tfn_000304",
            "loc_000302:",
            "\tbra.s\tloc_000302",
            "fn_000304:",
            "\tnop",
            "\trts",
        ]
    );
}

#[test]
fn test_listing_prints_word_offset_tables_symbolically() {
    let text = listing(&table_hints());
    let lines = code(&text);
    let start = lines
        .iter()
        .position(|line| *line == "tbl_000320:")
        .expect("table is labelled");
    assert_eq!(
        lines[start..start + 3],
        [
            "tbl_000320:",
            "\tdc.w\tfn_000330-tbl_000320",
            "\tdc.w\tfn_000332-tbl_000320",
        ]
    );
}

#[test]
fn test_listing_prints_unreached_code_as_data() {
    let text = listing(&table_hints());
    assert!(!text.contains("; 000340: "), "{text}");
    assert!(!text.contains("; 000342: "), "{text}");
}

#[test]
fn test_listing_has_header_footer_and_ascending_addresses() {
    let text = listing(&Hints::default());
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("; ROM SHA-1 "));
    assert_eq!(
        lines[1..4],
        ["\tcpu\t68000", "\tsupmode\ton", "\tpadding\toff"]
    );
    assert_eq!(lines.last(), Some(&"\tend"));
    let addresses: Vec<u32> = lines
        .iter()
        .filter_map(|line| line.split_once("; ").map(|(_, comment)| comment))
        .filter_map(|comment| u32::from_str_radix(comment.get(..6)?, 16).ok())
        .collect();
    assert!(addresses.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(addresses.first(), Some(&0));
    assert!(
        addresses
            .last()
            .is_some_and(|&last| last as usize >= ROM_SIZE - 16)
    );
}

#[test]
fn test_listing_prints_zero_displacement_as_words_with_text_in_comment() {
    let text = listing(&as_limit_hints());
    let line = text
        .lines()
        .find(|line| line.ends_with("; 000350: 1028 0000  move.b $0(a0),d0"))
        .unwrap_or_else(|| panic!("zero displacement line missing:\n{text}"));
    assert!(line.starts_with("\tdc.w\t$1028,$0000"), "{line}");
}

#[test]
fn test_listing_names_targets_inside_instructions_from_the_next_label() {
    let text = listing(&as_limit_hints());
    assert!(
        code(&text).contains(&"\tlea\tfn_000360-14(pc),a0"),
        "label-relative lea missing:\n{text}"
    );
}

#[test]
fn test_listing_instruction_comment_carries_address_and_words() {
    let text = listing(&Hints::default());
    let line = text
        .lines()
        .find(|line| line.starts_with("\tbsr.s"))
        .expect("bsr is printed");
    assert!(line.ends_with("; 000300: 6102"), "{line}");
}

#[test]
fn test_listing_line_map_points_at_each_instruction_line() {
    let rom = Rom::from_bytes(image()).expect("synthetic image loads");
    let analysis = analyze_with(&rom, &table_hints());
    let mut out = Vec::new();
    let lines = write_listing(&mut out, rom.bytes(), &analysis).expect("writes");
    let listing = String::from_utf8(out).expect("ASCII listing");
    let listing: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), analysis.instructions.len());
    for (address, line) in lines {
        let text = listing[line as usize - 1];
        let marker = format!("; {:06X}: ", address.get());
        assert!(text.contains(&marker), "line {line}: {text}");
        assert!(!text.starts_with("\tdc."), "line {line}: {text}");
    }
}
