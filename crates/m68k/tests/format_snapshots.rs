//! Expected text in both dialects for synthetic instructions, including the words the Musashi
//! differential leaves out (ILLEGAL, line A/F, the 0xFF branches) and listing-mode labels.

use anygenesis_m68k::{Dialect, FormatOptions, LocationLabels, decode};
use anygenesis_shared::Address;

/// Where every snapshot instruction sits.
const ADDRESS: u32 = 0x1000;

struct Snapshot {
    words: &'static [u16],
    motorola: &'static str,
    as_text: &'static str,
}

const fn snapshot(
    words: &'static [u16],
    motorola: &'static str,
    as_text: &'static str,
) -> Snapshot {
    Snapshot {
        words,
        motorola,
        as_text,
    }
}

// One row per instruction; rustfmt would spread most rows over five lines.
#[rustfmt::skip]
const PLAIN: &[Snapshot] = &[
    snapshot(&[0x3200], "move.w  d0, d1", "move.w\td0,d1"),
    snapshot(
        &[0x23FC, 0x1234, 0x5678, 0x00FF, 0x0000],
        "move.l  #$12345678, $ff0000.l",
        "move.l\t#$12345678,($FF0000).l",
    ),
    snapshot(&[0x1038, 0xF600], "move.b  $f600.w, d0", "move.b\t($FFFFF600).w,d0"),
    snapshot(&[0x42B8, 0x7FFE], "clr.l   $7ffe.w", "clr.l\t($7FFE).w"),
    snapshot(&[0x4EB9, 0x00A1, 0x0000], "jsr     $a10000.l", "jsr\t($A10000).l"),
    snapshot(&[0x41E8, 0x0010], "lea     ($10,a0), a0", "lea\t$10(a0),a0"),
    snapshot(&[0x3028, 0x0000], "move.w  ($0,a0), d0", "move.w\t$0(a0),d0"),
    snapshot(&[0x3037, 0x00F0], "move.w  (-$10,a7,d0.w), d0", "move.w\t-$10(sp,d0.w),d0"),
    snapshot(&[0x3030, 0x9800], "move.w  (a0,a1.l), d0", "move.w\t(a0,a1.l),d0"),
    snapshot(&[0x43FA, 0x0010], "lea     ($10,pc), a1", "lea\t$1012(pc),a1"),
    snapshot(&[0x323B, 0x0006], "move.w  ($6,pc,d0.w), d1", "move.w\t$1008(pc,d0.w),d1"),
    snapshot(&[0x4EFB, 0x1000], "jmp     (pc,d1.w)", "jmp\t$1002(pc,d1.w)"),
    snapshot(&[0x083A, 0x0003, 0x0010], "btst    #$3, ($10,pc)", "btst\t#$3,$1014(pc)"),
    snapshot(&[0x4CBA, 0x0001, 0x0010], "movem.w ($10,pc), d0", "movem.w\t$1014(pc),d0"),
    snapshot(
        &[0x31FA, 0x0010, 0x1234],
        "move.w  ($10,pc), $1234.w",
        "move.w\t$1012(pc),($1234).w",
    ),
    snapshot(&[0x6010], "bra     $1012", "bra.s\t$1012"),
    snapshot(&[0x6600, 0xFFFE], "bne     $1000", "bne.w\t$1000"),
    snapshot(&[0x6402], "bcc     $1004", "bcc.s\t$1004"),
    snapshot(&[0x6500, 0x0100], "bcs     $1102", "bcs.w\t$1102"),
    snapshot(&[0x6100, 0x0010], "bsr     $1012", "bsr.w\t$1012"),
    snapshot(&[0x66FF], "bne     $1001", "bne.s\t$1001"),
    snapshot(&[0x51C9, 0xFFFE], "dbra    d1, $1000", "dbf\td1,$1000"),
    snapshot(&[0x57C9, 0xFFFE], "dbeq    d1, $1000", "dbeq\td1,$1000"),
    snapshot(&[0x50C0], "st      d0", "st.b\td0"),
    snapshot(&[0x70FF], "moveq   #-$1, d0", "moveq\t#-$1,d0"),
    snapshot(&[0x5048], "addq.w  #8, a0", "addq.w\t#8,a0"),
    snapshot(&[0x0640, 0xFFFF], "addi.w  #-$1, d0", "addi.w\t#$FFFF,d0"),
    snapshot(
        &[0x0C80, 0x8000, 0x0000],
        "cmpi.l  #-$80000000, d0",
        "cmpi.l\t#$80000000,d0",
    ),
    snapshot(&[0x0240, 0xFF00], "andi.w  #$ff00, d0", "andi.w\t#$FF00,d0"),
    snapshot(&[0x023C, 0x00F0], "andi    #$f0, ccr", "andi.b\t#$F0,ccr"),
    snapshot(&[0x46FC, 0x2700], "move    #$2700, sr", "move.w\t#$2700,sr"),
    snapshot(&[0x44FC, 0x787F], "move    #$787f, ccr", "move.w\t#$787F,ccr"),
    snapshot(&[0x4E68], "move    usp, a0", "move.l\tusp,a0"),
    snapshot(
        &[0x48E7, 0xFFFE],
        "movem.l d0-d7/a0-a6, -(a7)",
        "movem.l\td0-d7/a0-a6,-(sp)",
    ),
    snapshot(&[0x4C98, 0x800D], "movem.w (a0)+, d0/d2-d3/a7", "movem.w\t(a0)+,d0/d2-d3/sp"),
    snapshot(&[0x4C90, 0x0000], "movem.w (a0), ", "dc.w\t$4C90,$0000"),
    snapshot(&[0x0188, 0xFF80], "movep.w d0, (-$80,a0)", "movep.w\td0,-$80(a0)"),
    snapshot(&[0x4E56, 0xFFF0], "link    a6, #-$10", "link.w\ta6,#-$10"),
    snapshot(&[0x4E5E], "unlk    a6", "unlk\ta6"),
    snapshot(&[0xC189], "exg     d0, a1", "exg\td0,a1"),
    snapshot(&[0x4840], "swap    d0", "swap\td0"),
    snapshot(&[0xC109], "abcd    -(a1), -(a0)", "abcd.b\t-(a1),-(a0)"),
    snapshot(&[0xE3D0], "lsl.w   (a0)", "lsl.w\t(a0)"),
    snapshot(&[0xE548], "lsl.w   #2, d0", "lsl.w\t#2,d0"),
    snapshot(&[0x08D0, 0x0007], "bset    #$7, (a0)", "bset\t#$7,(a0)"),
    snapshot(&[0x4E4F], "trap    #$f", "trap\t#$F"),
    snapshot(&[0x4E72, 0x2700], "stop    #$2700", "stop\t#$2700"),
    snapshot(&[0x4E75], "rts", "rts"),
    snapshot(&[0x4E71], "nop", "nop"),
    snapshot(&[0x4AFC], "illegal", "illegal"),
    snapshot(&[0xA123], "dc.w    $a123", "dc.w\t$A123"),
    snapshot(&[0xF200], "dc.w    $f200", "dc.w\t$F200"),
];

/// Branch and PC-relative targets under [`LocationLabels`].
#[rustfmt::skip]
const LABELLED: &[Snapshot] = &[
    snapshot(&[0x6010], "bra     loc_001012", "bra.s\tloc_001012"),
    snapshot(&[0x51C9, 0xFFFE], "dbra    d1, loc_001000", "dbf\td1,loc_001000"),
    snapshot(&[0x43FA, 0x0010], "lea     (loc_001012,pc), a1", "lea\tloc_001012(pc),a1"),
    snapshot(
        &[0x323B, 0x0006],
        "move.w  (loc_001008,pc,d0.w), d1",
        "move.w\tloc_001008(pc,d0.w),d1",
    ),
    snapshot(&[0x4EB9, 0x00A1, 0x0000], "jsr     $a10000.l", "jsr\t($A10000).l"),
];

fn check(snapshots: &[Snapshot], labelled: bool) -> Vec<String> {
    let mut failures = Vec::new();
    for snapshot in snapshots {
        let instruction = decode(snapshot.words)
            .unwrap_or_else(|error| panic!("{:04X?}: {error}", snapshot.words));
        for (dialect, expected) in [
            (Dialect::Motorola, snapshot.motorola),
            (Dialect::As, snapshot.as_text),
        ] {
            let mut options = FormatOptions::new(dialect);
            if labelled {
                options = options.with_labels(&LocationLabels);
            }
            let text = instruction
                .format(Address::new(ADDRESS), options)
                .to_string();
            if text != expected {
                failures.push(format!(
                    "{:04X?} {dialect:?}: got {text:?}, expected {expected:?}",
                    snapshot.words
                ));
            }
        }
    }
    failures
}

#[test]
fn test_format_snapshots_both_dialects_match() {
    let failures = check(PLAIN, false);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_format_snapshots_with_labels_match() {
    let failures = check(LABELLED, true);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_format_brief_extension_bits_8_to_10_ignored() {
    // A 68000 ignores the scale and full-format bits; Musashi would print `*8` or `[...]`.
    let plain = decode(&[0x3030, 0x1000]).expect("valid");
    let marked = decode(&[0x3030, 0x1700]).expect("valid");
    for dialect in [Dialect::Motorola, Dialect::As] {
        let options = FormatOptions::new(dialect);
        let address = Address::new(ADDRESS);
        assert_eq!(
            marked.format(address, options).to_string(),
            plain.format(address, options).to_string()
        );
    }
}
