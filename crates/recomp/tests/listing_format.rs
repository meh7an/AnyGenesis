//! The listing reader over synthetic lines in the AS listing layout.

mod listing;

use listing::Coverage;

/// A body line: line number, address, byte field, then the source from column 40.
fn body(number: u32, address: u32, field: &str, source: &str) -> String {
    format!("{number:>8}/{address:>8X} : {field:<20}{source}\r\n")
}

/// A continuation line: bytes without a line number or source.
fn continuation(address: u32, field: &str) -> String {
    format!("{:>9}{address:>8X} : {field}\r\n", "")
}

fn parse(lines: &[String]) -> listing::Listing {
    let mut text = String::from(" AS listing - Page 1\r\n\r\n");
    text.extend(lines.iter().cloned());
    match listing::parse(text.as_bytes()) {
        Ok(listing) => listing,
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn test_parse_instructions_keep_length_and_mnemonic() {
    let listing = parse(&[
        body(1, 0x200, "", "Start:"),
        body(2, 0x200, "4E71", "\t\tnop\t\t\t; comment"),
        body(3, 0x202, "4AB9 00A1 0008", "Next:\tTST.L\t($A10008).l"),
        body(4, 0x208, "60FE", "\t\t!bra.s\t*"),
    ]);
    let found: Vec<_> = listing
        .instructions
        .iter()
        .map(|(&address, listed)| (address, listed.length, listed.mnemonic.as_str()))
        .collect();
    assert_eq!(
        found,
        [(0x200, 2, "nop"), (0x202, 6, "tst.l"), (0x208, 2, "bra.s")]
    );
    assert_eq!(
        listing.labels.into_iter().collect::<Vec<_>>(),
        [0x200, 0x202]
    );
}

#[test]
fn test_parse_continuation_lines_extend_the_emit() {
    let listing = parse(&[
        body(
            1,
            0x100,
            "0102 0304 0506 0708",
            "\t\tdc.b\t1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16",
        ),
        continuation(0x108, "090A 0B0C 0D0E 0F10"),
        body(2, 0x110, "0000 0200", "\t\tdc.l\tStart"),
    ]);
    let data: Vec<_> = listing
        .data
        .values()
        .map(|data| (data.address, data.length, data.directive.as_str()))
        .collect();
    assert_eq!(data, [(0x100, 16, "dc.b"), (0x110, 4, "dc.l")]);
}

#[test]
fn test_parse_continuation_with_a_gap_is_an_error() {
    let mut text = body(1, 0x100, "0001 0002 0003 0004", "\t\tdc.w\t1,2,3,4,5");
    text.push_str(&continuation(0x10A, "0005"));
    assert!(listing::parse(text.as_bytes()).is_err());
}

#[test]
fn test_parse_z80_section_is_skipped_until_restore() {
    let listing = parse(&[
        body(1, 0x300, "", "\t\tsave"),
        body(2, 0x300, "", "\t\tcpu\tz80"),
        body(3, 0x0, "", "Z80Start:"),
        body(4, 0x0, "F3", "\t\tdi"),
        body(5, 0x1, "C3 00 00", "\t\tjp\tZ80Start"),
        body(6, 0x300, "", "\t\trestore"),
        body(7, 0x300, "4E75", "\t\trts"),
    ]);
    assert_eq!(
        listing.instructions.keys().copied().collect::<Vec<_>>(),
        [0x300]
    );
    assert!(listing.data.is_empty());
    assert!(listing.labels.is_empty());
}

#[test]
fn test_parse_marker_lines_and_assignments_emit_nothing() {
    let listing = parse(&[
        body(1, 0x400, "(MACRO)[400]", "Blob:\tincludeBlob\t\"file\""),
        body(2, 0x400, "=$FFFF8000", "Var:\tequ\t$FFFF8000"),
        body(3, 0x400, "4E75", "\t\trts"),
    ]);
    assert_eq!(listing.instructions.len(), 1);
    assert_eq!(listing.labels.into_iter().collect::<Vec<_>>(), [0x400]);
}

#[test]
fn test_parse_stops_at_the_symbol_table() {
    let mut lines = vec![body(1, 0x200, "4E71", "\t\tnop")];
    lines.push(String::from("  Symbol Table (* = unused):\r\n"));
    lines.push(body(2, 0x202, "4E71", "\t\tnop"));
    assert_eq!(parse(&lines).instructions.len(), 1);
}

#[test]
fn test_coverage_prefers_instructions_over_patch_bytes() {
    let listing = parse(&[
        body(1, 0x200, "3028 0001", "\t\tmove.w\t1(a0),d0"),
        body(1, 0x203, "00", "\t\t!dc.b\t0"),
        body(2, 0x204, "0001 0002", "Table:\tdc.w\t1,2"),
    ]);
    assert_eq!(listing.coverage(0x200), Coverage::InstructionStart);
    assert_eq!(listing.coverage(0x203), Coverage::InsideInstruction(0x200));
    assert!(matches!(listing.coverage(0x206), Coverage::Data(data) if data.address == 0x204));
    assert_eq!(listing.coverage(0x208), Coverage::Unlisted);
}

#[test]
fn test_is_instruction_tells_mnemonics_from_directives() {
    for mnemonic in [
        "bhs.s", "dbf", "dbra", "st", "shi", "exg.l", "movem.l", "bra.w",
    ] {
        assert!(listing::is_instruction(mnemonic), "{mnemonic}");
    }
    for directive in ["dc.w", "dcb.b", "even", "binclude", "cpu", "align"] {
        assert!(!listing::is_instruction(directive), "{directive}");
    }
}
