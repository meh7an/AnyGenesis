//! Our Motorola text against Musashi's disassembler text for every word Musashi calls valid.
//!
//! Both texts pass through the normalization Mehran signed off (PROGRESS.md, M1.3), and
//! nothing else:
//! - N1: ASCII lowercase (Musashi prints registers uppercase).
//! - N2: drop Musashi's helper comment, from the first `;` to the end (`; ($target)` after
//!   `(d16,PC)`, `; opcode 1010` after line A words), then trailing spaces.
//!
//! Exceptions, where our text follows the 68000 manual and Musashi's does not (signed off,
//! each checked against expected text below instead):
//! - MOVE to CCR with an immediate: Musashi prints only the low byte of the immediate word.
//! - MOVEP with a negative displacement: Musashi prints the displacement unsigned.
//!
//! Words Musashi calls invalid stay out as in the M1.2 exhaustive test (ILLEGAL, the 0xFF
//! branches); line F is skipped too, since Musashi renders some of it as FPU and PMMU text.

use anygenesis_m68k::{Dialect, FormatOptions, MAX_WORDS, decode};
use anygenesis_oracle::musashi::{disassemble_68000, is_valid_68000};
use anygenesis_shared::Address;

/// Placed so that no branch or PC-relative target wraps: Musashi computes targets in 32 bits
/// without the 24-bit address mask.
const ADDRESS: u32 = 0x1_0000;

/// Extension words after the opcode. Every word keeps bits 8-10 clear: a 68000 ignores them
/// in a brief extension word, while Musashi's disassembler prints a 68020 full format or
/// scale for them. Together they cover zero, positive and negative values, data and address
/// index registers of both sizes, and the -$8000 and -$80000000 edge values.
const EXTENSION_PATTERNS: [[u16; MAX_WORDS - 1]; 4] = [
    [0x0000, 0x0000, 0x0000, 0x0000],
    [0x787F, 0x787F, 0x787F, 0x787F],
    [0xF880, 0xF880, 0xF880, 0xF880],
    [0x8000, 0x0000, 0x80FE, 0x80FE],
];

/// At most this many mismatches are listed before the test fails.
const REPORTED_MISMATCHES: usize = 40;

/// Applies rules N2 and N1, the whole normalization. N2 trims trailing spaces on both sides
/// alike: an empty MOVEM list leaves `, ` before Musashi's comment and at the end of ours.
fn normalize(text: &str) -> String {
    let text = match text.find(';') {
        Some(comment) => &text[..comment],
        None => text,
    };
    text.trim_end().to_ascii_lowercase()
}

fn is_line_f(opcode: u16) -> bool {
    opcode & 0xF000 == 0xF000
}

/// The two signed-off exceptions, for the words where they change the text.
fn is_exception(opcode: u16, extension: u16) -> bool {
    let move_to_ccr_immediate = opcode == 0x44FC && extension & 0xFF00 != 0;
    let movep_negative = opcode & 0xF138 == 0x0108 && extension & 0x8000 != 0;
    move_to_ccr_immediate || movep_negative
}

fn instruction_words(opcode: u16, pattern: [u16; MAX_WORDS - 1]) -> [u16; MAX_WORDS] {
    let mut words = [opcode; MAX_WORDS];
    words[1..].copy_from_slice(&pattern);
    words
}

fn ours(words: &[u16]) -> String {
    let instruction = decode(words).unwrap_or_else(|error| panic!("{words:04X?}: {error}"));
    instruction
        .format(Address::new(ADDRESS), FormatOptions::new(Dialect::Motorola))
        .to_string()
}

fn musashi(words: &[u16]) -> String {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
    disassemble_68000(ADDRESS, &bytes).text
}

#[test]
fn test_format_every_valid_word_matches_musashi_text() {
    let mut mismatches = Vec::new();
    let mut compared = 0;
    for opcode in 0..=u16::MAX {
        if !is_valid_68000(opcode) || is_line_f(opcode) {
            continue;
        }
        for pattern in EXTENSION_PATTERNS {
            if is_exception(opcode, pattern[0]) {
                continue;
            }
            let words = instruction_words(opcode, pattern);
            let (ours, musashi) = (ours(&words), musashi(&words));
            compared += 1;
            if normalize(&ours) != normalize(&musashi) {
                mismatches.push(format!(
                    "{opcode:04X} {pattern:04X?}: ours {ours:?}, Musashi {musashi:?}"
                ));
            }
        }
    }

    println!("{compared} texts compared");
    assert!(
        mismatches.is_empty(),
        "{} mismatches, first {}:\n{}",
        mismatches.len(),
        REPORTED_MISMATCHES.min(mismatches.len()),
        mismatches[..REPORTED_MISMATCHES.min(mismatches.len())].join("\n")
    );
}

#[test]
fn test_format_move_to_ccr_exception_keeps_whole_immediate_word() {
    let words = [0x44FC, 0x787F];
    assert_eq!(ours(&words), "move    #$787f, ccr");
    // Guards the exception: if a Musashi update fixed this, the exception must go.
    assert_eq!(musashi(&words), "move    #$7f, CCR");
}

#[test]
fn test_format_movep_exception_prints_signed_displacement() {
    let words = [0x0188, 0xFF80];
    assert_eq!(ours(&words), "movep.w d0, (-$80,a0)");
    assert_eq!(musashi(&words), "movep.w D0, ($ff80,A0)");
}

#[test]
fn test_normalize_drops_helper_comment_and_case() {
    assert_eq!(
        normalize("lea     ($10,PC), A0; ($10012)"),
        "lea     ($10,pc), a0"
    );
    assert_eq!(normalize("dc.w    $a000; opcode 1010"), "dc.w    $a000");
    assert_eq!(normalize("movem.w (A0), ; ($10004)"), "movem.w (a0),");
    assert_eq!(normalize("movem.w (a0), "), "movem.w (a0),");
}
