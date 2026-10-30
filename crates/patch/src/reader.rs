//! Reading patch files: fixed-width big-endian fields (IPS), the variable-length numbers and
//! the CRC-32 footer UPS and BPS share.

use crate::{Format, PatchError, crc32::crc32};

/// A cursor over a patch's bytes; running out is [`PatchError::Truncated`].
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
    format: Format,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8], format: Format) -> Self {
        Self {
            rest: bytes,
            format,
        }
    }

    /// The bytes not read yet.
    pub(crate) fn rest(&self) -> &'a [u8] {
        self.rest
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], PatchError> {
        if len > self.rest.len() {
            return Err(PatchError::Truncated {
                format: self.format,
            });
        }
        let (taken, rest) = self.rest.split_at(len);
        self.rest = rest;
        Ok(taken)
    }

    pub(crate) fn byte(&mut self) -> Result<u8, PatchError> {
        Ok(self.take(1)?[0])
    }

    /// A big-endian field `len` bytes wide (at most 4).
    pub(crate) fn big_endian(&mut self, len: usize) -> Result<u32, PatchError> {
        Ok(self
            .take(len)?
            .iter()
            .fold(0, |value, &byte| value << 8 | u32::from(byte)))
    }

    /// A UPS or BPS number: 7 bits per byte, low bits first, the top bit set on the last
    /// byte, and one added per byte that is not the last, so every value has one encoding.
    pub(crate) fn number(&mut self) -> Result<u64, PatchError> {
        let format = self.format;
        let overflow = || PatchError::Malformed {
            format,
            reason: "a number does not fit 64 bits",
        };
        let mut value = 0u64;
        let mut shift = 1u64;
        loop {
            let byte = self.byte()?;
            value = u64::from(byte & 0x7F)
                .checked_mul(shift)
                .and_then(|bits| value.checked_add(bits))
                .ok_or_else(overflow)?;
            if byte & 0x80 != 0 {
                return Ok(value);
            }
            shift = shift.checked_mul(0x80).ok_or_else(overflow)?;
            value = value.checked_add(shift).ok_or_else(overflow)?;
        }
    }

    /// A number used as a length or position in an image.
    pub(crate) fn size(&mut self) -> Result<usize, PatchError> {
        let format = self.format;
        usize::try_from(self.number()?).map_err(|_| PatchError::Malformed {
            format,
            reason: "a size does not fit this machine",
        })
    }
}

/// The CRC-32s at the end of a UPS or BPS patch, each four bytes, little-endian.
#[derive(Debug)]
pub(crate) struct Footer {
    source: u32,
    target: u32,
}

impl Footer {
    const LEN: usize = 12;

    /// Splits `patch` into its body and footer, checking the patch's own CRC-32, which
    /// covers everything before it.
    pub(crate) fn split(patch: &[u8], format: Format) -> Result<(&[u8], Self), PatchError> {
        let Some(body_len) = patch.len().checked_sub(Self::LEN) else {
            return Err(PatchError::Truncated { format });
        };
        let word = |index: usize| {
            let start = body_len + index * 4;
            u32::from_le_bytes([
                patch[start],
                patch[start + 1],
                patch[start + 2],
                patch[start + 3],
            ])
        };
        let (expected, found) = (word(2), crc32(&patch[..patch.len() - 4]));
        if expected != found {
            return Err(PatchError::PatchCrc {
                format,
                expected,
                found,
            });
        }
        let footer = Self {
            source: word(0),
            target: word(1),
        };
        Ok((&patch[..body_len], footer))
    }

    /// Fails unless `image` is the patch's source, `size` bytes long.
    pub(crate) fn check_source(
        &self,
        format: Format,
        image: &[u8],
        size: u64,
    ) -> Result<(), PatchError> {
        let found = crc32(image);
        if found == self.target && found != self.source {
            return Err(PatchError::AlreadyPatched { format });
        }
        if found != self.source {
            return Err(PatchError::SourceCrc {
                format,
                expected: self.source,
                found,
            });
        }
        if u64::try_from(image.len()).ok() != Some(size) {
            return Err(PatchError::SourceSize {
                format,
                expected: size,
                found: image.len(),
            });
        }
        Ok(())
    }

    /// Fails unless `output` is the patch's target.
    pub(crate) fn check_target(&self, format: Format, output: &[u8]) -> Result<(), PatchError> {
        let found = crc32(output);
        if found != self.target {
            return Err(PatchError::TargetCrc {
                format,
                expected: self.target,
                found,
            });
        }
        Ok(())
    }
}

/// Writing UPS and BPS patches in tests.
#[cfg(test)]
pub(crate) mod encode {
    use crate::crc32::crc32;

    /// `value` as a UPS or BPS number, by the specifications' encoder.
    pub(crate) fn number(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let low = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                bytes.push(0x80 | low);
                return bytes;
            }
            bytes.push(low);
            value -= 1;
        }
    }

    /// `body` with the footer for `source` and `target` and its own CRC-32 appended.
    pub(crate) fn finish(mut body: Vec<u8>, source: &[u8], target: &[u8]) -> Vec<u8> {
        body.extend(crc32(source).to_le_bytes());
        body.extend(crc32(target).to_le_bytes());
        body.extend(crc32(&body).to_le_bytes());
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Result<u64, PatchError> {
        Reader::new(bytes, Format::Bps).number()
    }

    #[test]
    fn test_number_known_encodings_decode() {
        assert_eq!(encode::number(0), [0x80]);
        assert_eq!(encode::number(127), [0xFF]);
        assert_eq!(encode::number(128), [0x00, 0x80]);
        assert_eq!(encode::number(16_511), [0x7F, 0xFF]);
        assert_eq!(encode::number(16_512), [0x00, 0x00, 0x80]);
        assert_eq!(decode(&[0x00, 0x80]).expect("a number"), 128);
    }

    #[test]
    fn test_number_round_trips_across_widths() {
        let values = [
            0,
            1,
            127,
            128,
            129,
            16_511,
            16_512,
            0x40_0000,
            u64::from(u32::MAX),
        ];
        for value in values.into_iter().chain([u64::MAX]) {
            let bytes = encode::number(value);
            let mut reader = Reader::new(&bytes, Format::Ups);
            assert_eq!(reader.number().expect("a number"), value);
            assert!(reader.is_empty());
        }
    }

    #[test]
    fn test_number_unterminated_or_too_wide_refused() {
        assert!(matches!(decode(&[0x00]), Err(PatchError::Truncated { .. })));
        let wide = [0x7F; 11].into_iter().chain([0x80]).collect::<Vec<_>>();
        assert!(matches!(decode(&wide), Err(PatchError::Malformed { .. })));
    }

    #[test]
    fn test_footer_checks_patch_source_and_target() {
        let (source, target) = (b"source".as_slice(), b"target".as_slice());
        let patch = encode::finish(b"BPS1".to_vec(), source, target);
        let (body, footer) = Footer::split(&patch, Format::Bps).expect("intact");
        assert_eq!(body, b"BPS1");
        footer
            .check_source(Format::Bps, source, 6)
            .expect("the source");
        footer
            .check_target(Format::Bps, target)
            .expect("the target");
        assert!(matches!(
            footer.check_source(Format::Bps, target, 6),
            Err(PatchError::AlreadyPatched { .. })
        ));
        assert!(matches!(
            footer.check_source(Format::Bps, b"other", 5),
            Err(PatchError::SourceCrc { .. })
        ));
        assert!(matches!(
            footer.check_source(Format::Bps, source, 7),
            Err(PatchError::SourceSize { .. })
        ));
        assert!(matches!(
            footer.check_target(Format::Bps, source),
            Err(PatchError::TargetCrc { .. })
        ));
        let mut damaged = patch.clone();
        damaged[1] ^= 1;
        assert!(matches!(
            Footer::split(&damaged, Format::Bps),
            Err(PatchError::PatchCrc { .. })
        ));
        assert!(matches!(
            Footer::split(&patch[..11], Format::Bps),
            Err(PatchError::Truncated { .. })
        ));
    }
}
