//! Every possible first word against Musashi's 68000 validity table and disassembler length.
//!
//! Musashi differs from a real 68000 in two places, signed off by Mehran (PROGRESS.md, M1.2);
//! the decoder follows the 68000 manual there and this test checks it against the manual:
//! - Bcc/BRA/BSR with displacement byte 0xFF: Musashi maps them to the 68020 long branch and
//!   calls them invalid; on a 68000 they are 2-byte branches with displacement -1.
//! - Line F: Musashi calls some 0xFxxx words valid (FPU, PMMU, plain line F) and others
//!   invalid (68020 coprocessor, 68040 words), and reads PMMU extension words; a 68000 takes
//!   the line-F trap for all of them.

use anygenesis_m68k::{DecodeError, MAX_WORDS, Mnemonic, Operand, decode};
use anygenesis_oracle::musashi::{disassemble_68000, is_valid_68000};

/// Extension-word fillers. Both keep bit 8 clear: with it set, Musashi's disassembler takes
/// the 68020 full-format path regardless of the CPU type, which a 68000 never does.
const EXTENSION_PATTERNS: [u16; 2] = [0x0000, 0x7E7E];

/// Musashi lists at most this many mismatches before the test fails.
const REPORTED_MISMATCHES: usize = 40;

fn is_byte_branch_minus_one(opcode: u16) -> bool {
    opcode & 0xF0FF == 0x60FF
}

fn is_line_f(opcode: u16) -> bool {
    opcode & 0xF000 == 0xF000
}

/// Our answer in Musashi's terms: `Some(length)` for what it calls valid, `None` otherwise.
/// Musashi gives ILLEGAL its illegal-instruction handler, so it counts as invalid there.
fn our_classification(words: &[u16]) -> Option<u32> {
    match decode(words) {
        Ok(instruction) if instruction.mnemonic == Mnemonic::Illegal => None,
        Ok(instruction) => Some(instruction.length()),
        Err(DecodeError::InvalidOpcode { .. }) => None,
        Err(error) => panic!("{words:04X?}: {error}"),
    }
}

fn musashi_classification(opcode: u16, words: &[u16]) -> Option<u32> {
    if !is_valid_68000(opcode) {
        return None;
    }
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
    Some(disassemble_68000(0x1000, &bytes).length)
}

/// Checks a word Musashi gets wrong against the 68000 manual instead.
fn check_exception(opcode: u16, words: &[u16]) -> Result<(), String> {
    let instruction = decode(words).map_err(|error| format!("{opcode:04X}: {error}"))?;
    let expected_ok = if is_line_f(opcode) {
        instruction.mnemonic == Mnemonic::LineF
    } else {
        matches!(
            instruction.mnemonic,
            Mnemonic::Bra | Mnemonic::Bsr | Mnemonic::Bcc(_)
        ) && instruction.operands[0] == Some(Operand::Branch(-1))
    };
    if expected_ok && instruction.length() == 2 {
        Ok(())
    } else {
        Err(format!(
            "{opcode:04X}: exception decoded as {instruction:?}"
        ))
    }
}

#[test]
fn test_decode_every_first_word_matches_musashi_validity_and_length() {
    let mut mismatches = Vec::new();
    let mut valid = 0;
    for opcode in 0..=u16::MAX {
        for pattern in EXTENSION_PATTERNS {
            let mut words = [pattern; MAX_WORDS];
            words[0] = opcode;
            if is_line_f(opcode) || is_byte_branch_minus_one(opcode) {
                if let Err(mismatch) = check_exception(opcode, &words) {
                    mismatches.push(mismatch);
                }
                continue;
            }
            let ours = our_classification(&words);
            let musashi = musashi_classification(opcode, &words);
            if ours != musashi {
                mismatches.push(format!(
                    "{opcode:04X} (extension {pattern:04X}): ours {ours:?}, Musashi {musashi:?}"
                ));
            }
            if pattern == EXTENSION_PATTERNS[0] && ours.is_some() {
                valid += 1;
            }
        }
    }

    println!("{valid} first words valid outside the exceptions");
    assert!(
        mismatches.is_empty(),
        "{} mismatches, first {}:\n{}",
        mismatches.len(),
        REPORTED_MISMATCHES.min(mismatches.len()),
        mismatches[..REPORTED_MISMATCHES.min(mismatches.len())].join("\n")
    );
}

#[test]
fn test_musashi_byte_branch_minus_one_still_invalid() {
    // Guards the exception list: if a Musashi update fixed this, the exception must go.
    for condition in 0..16 {
        let opcode = 0x60FF | (condition << 8);
        assert!(!is_valid_68000(opcode), "{opcode:04X}");
    }
}
