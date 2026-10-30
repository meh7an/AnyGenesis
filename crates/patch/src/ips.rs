//! IPS: records of a 24-bit offset and the bytes to write there (or a run of one byte),
//! ended by "EOF" and an optional 24-bit length to truncate the image to. IPS carries no
//! checksum, so it applies to any image.

use crate::{Format, PatchError, check_size, reader::Reader};

pub(crate) const MAGIC: &[u8] = b"PATCH";

const EOF: &[u8] = b"EOF";
const FORMAT: Format = Format::Ips;

/// `image` with the IPS `patch` applied. Records may write past the image's end, which
/// grows it with zeros up to the 4 MiB limit; a truncation length after "EOF" only shrinks.
pub(crate) fn apply(image: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    let mut reader = Reader::new(&patch[MAGIC.len()..], FORMAT);
    let mut output = image.to_vec();
    // "EOF" where a record would start ends the records, which is why no record can start
    // at offset 0x454F46.
    while !reader.rest().starts_with(EOF) {
        let offset = reader.big_endian(3)? as usize;
        let size = reader.big_endian(2)? as usize;
        if size == 0 {
            let run = reader.big_endian(2)? as usize;
            let value = reader.byte()?;
            target(&mut output, offset, run)?.fill(value);
        } else {
            let bytes = reader.take(size)?;
            target(&mut output, offset, size)?.copy_from_slice(bytes);
        }
    }
    reader.take(EOF.len())?;
    match reader.rest().len() {
        0 => {}
        3 => {
            let len = reader.big_endian(3)? as usize;
            if len > output.len() {
                return Err(PatchError::Malformed {
                    format: FORMAT,
                    reason: "its truncation length is past the image's end",
                });
            }
            output.truncate(len);
        }
        _ => {
            return Err(PatchError::Malformed {
                format: FORMAT,
                reason: "bytes after EOF are not a 3-byte truncation length",
            });
        }
    }
    Ok(output)
}

/// The `len` bytes of `output` at `offset`, growing it with zeros to reach them.
fn target(output: &mut Vec<u8>, offset: usize, len: usize) -> Result<&mut [u8], PatchError> {
    let end = offset + len;
    if end > output.len() {
        output.resize(check_size(end as u64)?, 0);
    }
    Ok(&mut output[offset..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: &[u8] = b"0123456789";

    fn patch(records: &[u8]) -> Vec<u8> {
        [MAGIC, records].concat()
    }

    #[test]
    fn test_apply_literal_and_run_records_written() {
        let records = b"\x00\x00\x01\x00\x02ab\x00\x00\x05\x00\x00\x00\x03zEOF";
        let output = apply(IMAGE, &patch(records)).expect("applies");
        assert_eq!(output, b"0ab34zzz89");
    }

    #[test]
    fn test_apply_record_past_the_end_grows_the_image() {
        let output = apply(IMAGE, &patch(b"\x00\x00\x0C\x00\x01!EOF")).expect("applies");
        assert_eq!(output, b"0123456789\0\0!");
        let output = apply(IMAGE, &patch(b"EOF")).expect("applies");
        assert_eq!(output, IMAGE);
    }

    #[test]
    fn test_apply_truncation_after_eof_shrinks_only() {
        let output = apply(IMAGE, &patch(b"EOF\x00\x00\x04")).expect("applies");
        assert_eq!(output, b"0123");
        let output = apply(IMAGE, &patch(b"\x00\x00\x0A\x00\x01!EOF\x00\x00\x0B"));
        assert_eq!(output.expect("applies"), b"0123456789!");
        let error = apply(IMAGE, &patch(b"EOF\x00\x00\x0B")).expect_err("past the end");
        assert!(matches!(error, PatchError::Malformed { .. }), "{error}");
    }

    #[test]
    fn test_apply_bad_endings_refused() {
        for records in [&b""[..], b"\x00\x00\x01\x00\x02a", b"\x00\x00", b"EO"] {
            let error = apply(IMAGE, &patch(records)).expect_err("truncated");
            assert!(
                matches!(error, PatchError::Truncated { .. }),
                "{records:?}: {error}"
            );
        }
        for records in [&b"EOF\x00"[..], b"EOF\x00\x00\x01\x00"] {
            let error = apply(IMAGE, &patch(records)).expect_err("junk after EOF");
            assert!(
                matches!(error, PatchError::Malformed { .. }),
                "{records:?}: {error}"
            );
        }
    }

    #[test]
    fn test_apply_growth_past_4_mib_refused() {
        let error = apply(IMAGE, &patch(b"\x40\x00\x00\x00\x01!EOF")).expect_err("too large");
        assert!(matches!(error, PatchError::TooLarge { .. }), "{error}");
    }
}
