use std::ops::RangeInclusive;

use anygenesis_shared::{BoundsError, read_u16_be, read_u32_be};

/// Where the cartridge header starts; the 68000 vector table fills everything before it.
pub(crate) const HEADER_START: usize = 0x100;
/// Where the header ends and code begins; the checksum covers everything from here.
pub(crate) const HEADER_END: usize = 0x200;

const SYSTEM_TYPE: Field = Field::new(0x100, 16);
const COPYRIGHT: Field = Field::new(0x110, 16);
const DOMESTIC_TITLE: Field = Field::new(0x120, 48);
const OVERSEAS_TITLE: Field = Field::new(0x150, 48);
const SERIAL: Field = Field::new(0x180, 14);
const CHECKSUM: usize = 0x18E;
const ROM_RANGE: usize = 0x1A0;
const RAM_RANGE: usize = 0x1A8;
const REGION: Field = Field::new(0x1F0, 3);

/// The TMSS boot ROM accepts either spelling at the start of the header.
const SIGNATURES: [&[u8]; 2] = [b"SEGA", b" SEGA"];

#[derive(Debug, Clone, Copy)]
struct Field {
    offset: usize,
    len: usize,
}

impl Field {
    const fn new(offset: usize, len: usize) -> Self {
        Self { offset, len }
    }
}

/// The cartridge header at 0x100-0x1FF.
///
/// Text fields are decoded as ASCII with other bytes shown as `?`, then trimmed; titles of
/// Japanese releases may hold Shift-JIS, which this does not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Console name, such as `SEGA MEGA DRIVE` or `SEGA GENESIS`.
    pub system_type: String,
    /// Publisher and release date.
    pub copyright: String,
    /// Title shown in Japan.
    pub domestic_title: String,
    /// Title shown everywhere else.
    pub overseas_title: String,
    /// Product type, code and revision, such as `GM 00001009-00`.
    pub serial: String,
    /// The checksum the header claims, compared against [`crate::Rom::computed_checksum`].
    pub checksum: u16,
    /// Cartridge ROM addresses the header claims, inclusive.
    pub rom_range: RangeInclusive<u32>,
    /// Work RAM addresses the header claims, inclusive.
    pub ram_range: RangeInclusive<u32>,
    /// The raw region code, such as `JUE` or `4`.
    pub region_code: String,
    /// The decoded region code, or `None` when it is in neither known style.
    pub regions: Option<Regions>,
}

impl Header {
    /// Parses the header of a ROM image.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, BoundsError> {
        let region_code = text(bytes, REGION)?;
        Ok(Self {
            system_type: text(bytes, SYSTEM_TYPE)?,
            copyright: text(bytes, COPYRIGHT)?,
            domestic_title: text(bytes, DOMESTIC_TITLE)?,
            overseas_title: text(bytes, OVERSEAS_TITLE)?,
            serial: text(bytes, SERIAL)?,
            checksum: read_u16_be(bytes, CHECKSUM)?,
            rom_range: range(bytes, ROM_RANGE)?,
            ram_range: range(bytes, RAM_RANGE)?,
            regions: Regions::decode(&region_code),
            region_code,
        })
    }
}

/// The regions a cartridge supports, from either region-code style.
///
/// The old style lists the letters `J`, `U` and `E`. The newer style is one hex digit whose
/// bits are Japan (1), Asia PAL (2), Americas (4) and Europe (8). A lone `E` reads as the
/// letter, as most emulators do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regions {
    /// Japan, NTSC.
    pub japan: bool,
    /// Domestic PAL (Asia); only the hex style can express it.
    pub asia: bool,
    /// Americas, NTSC.
    pub americas: bool,
    /// Europe, PAL.
    pub europe: bool,
}

impl Regions {
    fn decode(code: &str) -> Option<Self> {
        Self::decode_letters(code).or_else(|| Self::decode_hex(code))
    }

    fn decode_letters(code: &str) -> Option<Self> {
        let mut regions = Self::default();
        for letter in code.chars().filter(|c| *c != ' ') {
            match letter {
                'J' => regions.japan = true,
                'U' => regions.americas = true,
                'E' => regions.europe = true,
                _ => return None,
            }
        }
        (regions != Self::default()).then_some(regions)
    }

    fn decode_hex(code: &str) -> Option<Self> {
        let mut digits = code.chars().filter(|c| *c != ' ');
        let bits = digits.next()?.to_digit(16)?;
        if digits.next().is_some() || bits == 0 {
            return None;
        }
        Some(Self {
            japan: bits & 1 != 0,
            asia: bits & 2 != 0,
            americas: bits & 4 != 0,
            europe: bits & 8 != 0,
        })
    }
}

/// Returns whether the header starts with a signature the TMSS boot ROM accepts.
pub(crate) fn has_signature(header_start: &[u8]) -> bool {
    SIGNATURES
        .iter()
        .any(|signature| header_start.starts_with(signature))
}

fn text(bytes: &[u8], field: Field) -> Result<String, BoundsError> {
    let raw = bytes
        .get(field.offset..field.offset + field.len)
        .ok_or(BoundsError {
            offset: field.offset,
            width: field.len,
            available: bytes.len(),
        })?;
    let decoded: String = raw
        .iter()
        .map(|&byte| match byte {
            0x20..=0x7E => char::from(byte),
            _ => '?',
        })
        .collect();
    Ok(decoded.trim().to_owned())
}

fn range(bytes: &[u8], offset: usize) -> Result<RangeInclusive<u32>, BoundsError> {
    Ok(read_u32_be(bytes, offset)?..=read_u32_be(bytes, offset + 4)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions(japan: bool, asia: bool, americas: bool, europe: bool) -> Option<Regions> {
        Some(Regions {
            japan,
            asia,
            americas,
            europe,
        })
    }

    #[test]
    fn test_regions_decode_letters_any_order_sets_each() {
        assert_eq!(Regions::decode("JUE"), regions(true, false, true, true));
        assert_eq!(Regions::decode("U  "), regions(false, false, true, false));
        assert_eq!(Regions::decode("EJ"), regions(true, false, false, true));
    }

    #[test]
    fn test_regions_decode_lone_e_reads_as_letter() {
        assert_eq!(Regions::decode("E  "), regions(false, false, false, true));
    }

    #[test]
    fn test_regions_decode_hex_digit_sets_bits() {
        assert_eq!(Regions::decode("4  "), regions(false, false, true, false));
        assert_eq!(Regions::decode("F"), regions(true, true, true, true));
        assert_eq!(Regions::decode("A"), regions(false, true, false, true));
    }

    #[test]
    fn test_regions_decode_unknown_code_none() {
        assert_eq!(Regions::decode("   "), None);
        assert_eq!(Regions::decode("0"), None);
        assert_eq!(Regions::decode("JX"), None);
        assert_eq!(Regions::decode("45"), None);
    }

    #[test]
    fn test_has_signature_both_spellings_accepted() {
        assert!(has_signature(b"SEGA GENESIS    "));
        assert!(has_signature(b" SEGA MEGA DRIVE"));
        assert!(!has_signature(b"sega genesis    "));
    }

    #[test]
    fn test_text_non_ascii_replaced_and_trimmed() {
        let mut bytes = vec![b' '; 0x200];
        bytes[0x120..0x126].copy_from_slice(b"AB\x82\x00CD");
        assert_eq!(text(&bytes, DOMESTIC_TITLE), Ok("AB??CD".to_owned()));
    }
}
