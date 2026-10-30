//! BPS: the source, target and metadata sizes, the metadata, then actions that build the
//! target from the source, the patch and the target written so far, then the source, target
//! and patch CRC-32s.

use crate::{
    Format, PatchError, check_size,
    reader::{Footer, Reader},
};

pub(crate) const MAGIC: &[u8] = b"BPS1";

const FORMAT: Format = Format::Bps;

const SOURCE_READ: u64 = 0;
const TARGET_READ: u64 = 1;
const SOURCE_COPY: u64 = 2;

/// `image` with the BPS `patch` applied, after checking `image` is the patch's source.
pub(crate) fn apply(image: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchError> {
    let (body, footer) = Footer::split(patch, FORMAT)?;
    let mut reader = Reader::new(&body[MAGIC.len()..], FORMAT);
    let source_size = reader.number()?;
    let target_size = check_size(reader.number()?)?;
    let metadata_size = reader.size()?;
    reader.take(metadata_size)?;
    footer.check_source(FORMAT, image, source_size)?;
    let mut target = Vec::with_capacity(target_size);
    let (mut source_cursor, mut target_cursor) = (0usize, 0usize);
    while !reader.is_empty() {
        let action = reader.number()?;
        let len = usize::try_from((action >> 2) + 1)
            .ok()
            .filter(|&len| len <= target_size - target.len())
            .ok_or_else(|| malformed("an action writes past the target's end"))?;
        match action & 3 {
            SOURCE_READ => {
                let start = target.len();
                target.extend_from_slice(source(image, start, len)?);
            }
            TARGET_READ => target.extend_from_slice(reader.take(len)?),
            SOURCE_COPY => {
                source_cursor = moved(source_cursor, reader.number()?)?;
                target.extend_from_slice(source(image, source_cursor, len)?);
                source_cursor += len;
            }
            _ => {
                target_cursor = moved(target_cursor, reader.number()?)?;
                if target_cursor >= target.len() {
                    return Err(malformed("a target copy reads what is not written yet"));
                }
                // Byte by byte: the copy may read what it has just written, repeating it.
                for _ in 0..len {
                    target.push(target[target_cursor]);
                    target_cursor += 1;
                }
            }
        }
    }
    if target.len() != target_size {
        return Err(malformed("the actions end before the target is complete"));
    }
    footer.check_target(FORMAT, &target)?;
    Ok(target)
}

/// The `len` source bytes at `start`.
fn source(image: &[u8], start: usize, len: usize) -> Result<&[u8], PatchError> {
    start
        .checked_add(len)
        .and_then(|end| image.get(start..end))
        .ok_or_else(|| malformed("an action reads past the source's end"))
}

/// `cursor` moved by a relative offset: its magnitude above bit 0, negative when bit 0 is
/// set.
fn moved(cursor: usize, offset: u64) -> Result<usize, PatchError> {
    let magnitude = usize::try_from(offset >> 1).ok();
    let cursor = if offset & 1 == 0 {
        magnitude.and_then(|magnitude| cursor.checked_add(magnitude))
    } else {
        magnitude.and_then(|magnitude| cursor.checked_sub(magnitude))
    };
    cursor.ok_or_else(|| malformed("a copy moves its cursor out of range"))
}

fn malformed(reason: &'static str) -> PatchError {
    PatchError::Malformed {
        format: FORMAT,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::encode;

    const TARGET_COPY: u64 = 3;
    const SOURCE: &[u8] = b"Hello, world! Goodbye.";

    /// One action's bytes: its command and length, then `extra` (an offset or literals).
    fn action(command: u64, len: u64, extra: &[u8]) -> Vec<u8> {
        [encode::number((len - 1) << 2 | command), extra.to_vec()].concat()
    }

    fn offset(delta: i64) -> Vec<u8> {
        encode::number(delta.unsigned_abs() << 1 | u64::from(delta < 0))
    }

    fn patch(source: &[u8], target: &[u8], actions: &[Vec<u8>]) -> Vec<u8> {
        let mut body = MAGIC.to_vec();
        body.extend(encode::number(source.len() as u64));
        body.extend(encode::number(target.len() as u64));
        body.extend(encode::number(4));
        body.extend(b"meta");
        body.extend(actions.concat());
        encode::finish(body, source, target)
    }

    /// "Hello, there! Hello!!!!" from SOURCE with every action.
    fn every_action() -> (Vec<u8>, Vec<u8>) {
        let target = b"Hello, there! Hello!!!!".to_vec();
        let actions = [
            action(SOURCE_READ, 7, &[]),
            action(TARGET_READ, 5, b"there"),
            action(SOURCE_COPY, 2, &offset(12)),
            action(SOURCE_COPY, 5, &offset(-14)),
            action(TARGET_COPY, 1, &offset(12)),
            action(TARGET_COPY, 3, &offset(6)),
        ];
        (patch(SOURCE, &target, &actions), target)
    }

    #[test]
    fn test_apply_every_action_builds_the_target() {
        let (patch, target) = every_action();
        assert_eq!(apply(SOURCE, &patch).expect("applies"), target);
    }

    #[test]
    fn test_apply_wrong_or_patched_source_refused() {
        let (patch, target) = every_action();
        let error = apply(b"Hello", &patch).expect_err("wrong source");
        assert!(matches!(error, PatchError::SourceCrc { .. }), "{error}");
        let error = apply(&target, &patch).expect_err("already patched");
        assert!(
            matches!(error, PatchError::AlreadyPatched { .. }),
            "{error}"
        );
    }

    #[test]
    fn test_apply_out_of_range_actions_refused() {
        let target = b"Hello".to_vec();
        let cases = [
            vec![action(SOURCE_READ, 6, &[])],
            vec![action(SOURCE_READ, 3, &[])],
            vec![action(SOURCE_COPY, 5, &offset(-1))],
            vec![action(SOURCE_COPY, 5, &offset(20))],
            vec![action(TARGET_COPY, 5, &offset(0))],
        ];
        for actions in cases {
            let error = apply(SOURCE, &patch(SOURCE, &target, &actions)).expect_err("refused");
            assert!(matches!(error, PatchError::Malformed { .. }), "{error}");
        }
        let short = patch(SOURCE, &target, &[action(TARGET_READ, 5, b"Hel")]);
        let error = apply(SOURCE, &short).expect_err("literals run out");
        assert!(matches!(error, PatchError::Truncated { .. }), "{error}");
    }
}
