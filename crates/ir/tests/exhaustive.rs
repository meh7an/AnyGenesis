//! Every instruction the decoder accepts lifts to a well-formed body.
//!
//! All 65,536 first words, each with M1.3's four extension-word patterns (zero, positive and
//! negative values, data and address index registers of both sizes, the -$8000 and
//! -$80000000 edges). Bits 8-10 of the brief extension words do not matter: the decoder
//! ignores them.

use anygenesis_ir::{Control, Op, exception_entry, lift, validate};
use anygenesis_m68k::{MAX_WORDS, decode};
use anygenesis_shared::Address;

const ADDRESS: u32 = 0x1_0000;

const EXTENSION_PATTERNS: [[u16; MAX_WORDS - 1]; 4] = [
    [0x0000, 0x0000, 0x0000, 0x0000],
    [0x787F, 0x787F, 0x787F, 0x787F],
    [0xF880, 0xF880, 0xF880, 0xF880],
    [0x8000, 0x0000, 0x80FE, 0x80FE],
];

#[test]
fn test_lift_every_decoded_instruction_is_valid() {
    let mut lifted = 0;
    let mut vectors = Vec::new();
    for opcode in 0..=u16::MAX {
        for pattern in EXTENSION_PATTERNS {
            let mut words = [opcode; MAX_WORDS];
            words[1..].copy_from_slice(&pattern);
            let Ok(instruction) = decode(&words) else {
                continue;
            };
            let result = lift(&instruction, Address::new(ADDRESS))
                .unwrap_or_else(|error| panic!("{words:04X?}: {error}"));
            validate(&result.body)
                .unwrap_or_else(|error| panic!("{words:04X?}: {error}\n{result}"));
            for op in &result.body.ops {
                if let Op::Branch {
                    control: Control::Exception { vector, .. },
                    ..
                }
                | Op::Exit(Control::Exception { vector, .. }) = op
                {
                    vectors.push(*vector);
                }
            }
            lifted += 1;
        }
    }
    // 49,895 Musashi-valid first words, the 16 0xFF branches, line F (4,096) and ILLEGAL,
    // with four patterns each.
    assert_eq!(lifted, (49_895 + 16 + 4_096 + 1) * 4);
    vectors.sort_unstable();
    vectors.dedup();
    for vector in vectors {
        let entry = exception_entry(vector, Address::new(ADDRESS));
        validate(&entry).unwrap_or_else(|error| panic!("vector {vector}: {error}\n{entry}"));
    }
}
