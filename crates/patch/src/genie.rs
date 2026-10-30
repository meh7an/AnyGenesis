//! Genesis Game Genie codes: eight characters (ABCD-EFGH) of five bits each, whose 40 bits
//! hold a 24-bit address and the 16-bit word to put there in a fixed shuffle. The device
//! replaces the word whenever the game reads it; we write it into the image instead.

use std::{fmt, str::FromStr};

use crate::{PatchError, overwrite};

/// Each character's value is its index here; I, O, Q and U are left out.
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPRSTVWXYZ0123456789";
const LEN: usize = 8;
const BITS: u32 = 40;

/// The code bits, numbered 1 to 40 from the top, that make up the address and the word,
/// highest bit first.
const ADDRESS_BITS: [u32; 24] = [
    17, 18, 19, 20, 21, 22, 23, 24, 9, 10, 11, 12, 13, 14, 15, 16, 33, 34, 35, 36, 37, 38, 39, 40,
];
const VALUE_BITS: [u32; 16] = [30, 31, 32, 25, 26, 27, 28, 29, 1, 2, 3, 4, 5, 6, 7, 8];

/// A decoded Game Genie code: the word `value` at `address`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenieCode {
    pub address: u32,
    pub value: u16,
}

impl GenieCode {
    /// Writes the code's word into `image`.
    ///
    /// # Errors
    /// [`PatchError::OutOfImage`] when the word lies past the image's end.
    pub fn apply(&self, image: &mut [u8]) -> Result<(), PatchError> {
        overwrite(image, self.address as usize, &self.value.to_be_bytes())
    }
}

/// Reads a code in either case, with or without the hyphen after its fourth character. A
/// code for an odd address is refused: the word it replaces could not be read there.
impl FromStr for GenieCode {
    type Err = PatchError;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        let error = |reason| PatchError::Genie {
            code: code.to_owned(),
            reason,
        };
        let characters: Vec<u8> = match code.as_bytes() {
            [head @ .., b'-', _, _, _, _] if head.len() == 4 => {
                code.bytes().filter(|&byte| byte != b'-').collect()
            }
            bytes => bytes.to_vec(),
        };
        if characters.len() != LEN {
            return Err(error("is not eight characters (ABCD-EFGH)"));
        }
        let bits = characters.iter().try_fold(0u64, |bits, character| {
            ALPHABET
                .iter()
                .position(|letter| *letter == character.to_ascii_uppercase())
                .map(|value| bits << 5 | value as u64)
                .ok_or_else(|| error("holds a character outside the Game Genie alphabet"))
        })?;
        let code = Self {
            address: pick(bits, &ADDRESS_BITS),
            value: pick(bits, &VALUE_BITS) as u16,
        };
        if code.address & 1 != 0 {
            return Err(error("is for an odd address"));
        }
        Ok(code)
    }
}

impl fmt::Display for GenieCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:06X}:{:04X}", self.address, self.value)
    }
}

/// The code bits at `numbers` (1 to 40, from the top), highest first, as one number.
fn pick(bits: u64, numbers: &[u32]) -> u32 {
    numbers.iter().fold(0, |value, number| {
        value << 1 | ((bits >> (BITS - number)) & 1) as u32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code for `address` and `value`, by the same shuffle run backwards.
    fn encode(address: u32, value: u16) -> String {
        let mut bits = 0u64;
        let mut place = |numbers: &[u32], field: u32| {
            for (index, number) in numbers.iter().enumerate() {
                let bit = (field >> (numbers.len() - 1 - index)) & 1;
                bits |= u64::from(bit) << (BITS - number);
            }
        };
        place(&ADDRESS_BITS, address);
        place(&VALUE_BITS, u32::from(value));
        let characters: String = (0..LEN)
            .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 0x1F) as usize] as char)
            .collect();
        format!("{}-{}", &characters[..4], &characters[4..])
    }

    fn decode(code: &str) -> GenieCode {
        code.parse().expect("a valid code")
    }

    #[test]
    fn test_parse_published_example_decodes() {
        let code = decode("SCRA-BAX0");
        assert_eq!(
            code,
            GenieCode {
                address: 0x00_9C76,
                value: 0x5078
            }
        );
        assert_eq!(code.to_string(), "009C76:5078");
        assert_eq!(decode("scrabax0"), code);
    }

    #[test]
    fn test_parse_round_trips_through_the_shuffle() {
        let cases = [
            (0, 0),
            (0x3F_FFFE, 0xFFFF),
            (0x12_3456, 0xBEEF),
            (0x00_0200, 0x4E71),
        ];
        for (address, value) in cases {
            let code = encode(address, value);
            assert_eq!(decode(&code), GenieCode { address, value }, "{code}");
        }
        assert_eq!(encode(0x00_9C76, 0x5078), "SCRA-BAX0");
    }

    #[test]
    fn test_parse_bad_codes_refused() {
        let odd = encode(0x00_0201, 0x4E71);
        for code in [
            "SCRA-BAX",
            "SCRA-BAX00",
            "SCRAB-AX0",
            "SCRA-BAXO",
            "SCRI-BAX0",
            &odd,
        ] {
            let error = code.parse::<GenieCode>().expect_err("refused");
            assert!(matches!(error, PatchError::Genie { .. }), "{code}: {error}");
        }
    }

    #[test]
    fn test_apply_writes_the_word_inside_the_image_only() {
        let mut image = vec![0; 0x10];
        GenieCode {
            address: 0x0E,
            value: 0x4E71,
        }
        .apply(&mut image)
        .expect("inside");
        assert_eq!(&image[0x0E..], [0x4E, 0x71]);
        let error = GenieCode {
            address: 0x10,
            value: 0,
        }
        .apply(&mut image)
        .expect_err("past");
        assert!(matches!(error, PatchError::OutOfImage { .. }), "{error}");
    }
}
