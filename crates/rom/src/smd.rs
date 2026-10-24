//! Detection of interleaved `.smd` dumps, which are refused rather than converted.
//!
//! An `.smd` file is a 512-byte copier header followed by 16 KiB blocks, each holding the
//! block's odd bytes first and its even bytes second.

use crate::header::{self, HEADER_START};

pub(crate) const COPIER_HEADER: usize = 512;
pub(crate) const BLOCK: usize = 16 * 1024;
const HALF_BLOCK: usize = BLOCK / 2;

/// Copier header bytes 8 and 9 on files written by Super Magic Drive copiers.
const COPIER_MAGIC: [u8; 2] = [0xAA, 0xBB];
const COPIER_MAGIC_OFFSET: usize = 8;

/// Enough of the header to hold either signature spelling.
const SIGNATURE_LEN: usize = 5;

/// Returns whether `bytes` looks like an interleaved `.smd` dump instead of a raw `.bin`.
pub(crate) fn is_interleaved(bytes: &[u8]) -> bool {
    if header::has_signature(&bytes[HEADER_START.min(bytes.len())..]) {
        return false;
    }
    let Some(first_block) = bytes.get(COPIER_HEADER..COPIER_HEADER + BLOCK) else {
        return false;
    };
    if !(bytes.len() - COPIER_HEADER).is_multiple_of(BLOCK) {
        return false;
    }
    let has_magic = bytes[COPIER_MAGIC_OFFSET..].starts_with(&COPIER_MAGIC);
    has_magic || header::has_signature(&deinterleaved_signature(first_block))
}

/// De-interleaves just the bytes where a raw image's header signature would sit.
fn deinterleaved_signature(block: &[u8]) -> [u8; SIGNATURE_LEN] {
    let mut signature = [0; SIGNATURE_LEN];
    for (index, byte) in signature.iter_mut().enumerate() {
        let offset = HEADER_START + index;
        // Even bytes live in the second half of the block.
        let half = HALF_BLOCK * usize::from(offset.is_multiple_of(2));
        *byte = block[half + offset / 2];
    }
    signature
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Interleaves a raw image into `.smd` layout behind a zeroed copier header.
    pub(crate) fn interleave(raw: &[u8], copier_magic: bool) -> Vec<u8> {
        let mut smd = vec![0; COPIER_HEADER];
        if copier_magic {
            smd[COPIER_MAGIC_OFFSET..COPIER_MAGIC_OFFSET + 2].copy_from_slice(&COPIER_MAGIC);
        }
        for block in raw.chunks(BLOCK) {
            smd.extend(block.iter().skip(1).step_by(2));
            smd.extend(block.iter().step_by(2));
        }
        smd
    }

    fn raw_image() -> Vec<u8> {
        let mut raw = vec![0; BLOCK * 2];
        raw[HEADER_START..HEADER_START + 4].copy_from_slice(b"SEGA");
        raw
    }

    #[test]
    fn test_is_interleaved_raw_image_false() {
        assert!(!is_interleaved(&raw_image()));
    }

    #[test]
    fn test_is_interleaved_without_copier_magic_found_by_signature() {
        assert!(is_interleaved(&interleave(&raw_image(), false)));
    }

    #[test]
    fn test_is_interleaved_copier_magic_without_signature_true() {
        assert!(is_interleaved(&interleave(&[0; BLOCK], true)));
    }

    #[test]
    fn test_is_interleaved_partial_block_false() {
        let mut smd = interleave(&raw_image(), true);
        smd.truncate(smd.len() - 2);
        assert!(!is_interleaved(&smd));
    }

    #[test]
    fn test_is_interleaved_short_input_false() {
        assert!(!is_interleaved(&[0xAA; 16]));
    }
}
