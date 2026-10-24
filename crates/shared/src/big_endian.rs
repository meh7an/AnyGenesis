use crate::Size;

/// A read that runs past the end of a byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("reading {width} bytes at offset {offset:#x} runs past the end of {available} bytes")]
pub struct BoundsError {
    /// Where the read started.
    pub offset: usize,
    /// How many bytes it needed.
    pub width: usize,
    /// How many bytes the slice holds.
    pub available: usize,
}

/// Reads the byte at `offset`.
///
/// # Errors
/// Returns [`BoundsError`] when `offset` is past the end of `bytes`.
pub fn read_u8(bytes: &[u8], offset: usize) -> Result<u8, BoundsError> {
    read_array::<1>(bytes, offset).map(|[byte]| byte)
}

/// Reads the big-endian word at `offset`.
///
/// # Errors
/// Returns [`BoundsError`] when the word does not fit in `bytes`.
pub fn read_u16_be(bytes: &[u8], offset: usize) -> Result<u16, BoundsError> {
    read_array(bytes, offset).map(u16::from_be_bytes)
}

/// Reads the big-endian long word at `offset`.
///
/// # Errors
/// Returns [`BoundsError`] when the long word does not fit in `bytes`.
pub fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, BoundsError> {
    read_array(bytes, offset).map(u32::from_be_bytes)
}

/// Reads a big-endian operand of `size` at `offset`, zero-extended to 32 bits.
///
/// # Errors
/// Returns [`BoundsError`] when the operand does not fit in `bytes`.
pub fn read_sized_be(bytes: &[u8], offset: usize, size: Size) -> Result<u32, BoundsError> {
    match size {
        Size::Byte => read_u8(bytes, offset).map(u32::from),
        Size::Word => read_u16_be(bytes, offset).map(u32::from),
        Size::Long => read_u32_be(bytes, offset),
    }
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], BoundsError> {
    offset
        .checked_add(N)
        .and_then(|end| bytes.get(offset..end))
        .and_then(|slice| slice.try_into().ok())
        .ok_or(BoundsError {
            offset,
            width: N,
            available: bytes.len(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BYTES: [u8; 6] = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];

    #[test]
    fn test_read_u16_be_in_bounds_high_byte_first() {
        assert_eq!(read_u16_be(&BYTES, 0), Ok(0x1234));
        assert_eq!(read_u16_be(&BYTES, 3), Ok(0x789A));
    }

    #[test]
    fn test_read_u32_be_in_bounds_high_byte_first() {
        assert_eq!(read_u32_be(&BYTES, 2), Ok(0x5678_9ABC));
    }

    #[test]
    fn test_read_u32_be_past_end_returns_bounds_error() {
        let expected = BoundsError {
            offset: 3,
            width: 4,
            available: 6,
        };
        assert_eq!(read_u32_be(&BYTES, 3), Err(expected));
    }

    #[test]
    fn test_read_u8_offset_overflow_returns_bounds_error() {
        assert!(read_u8(&BYTES, usize::MAX).is_err());
    }

    #[test]
    fn test_read_sized_be_each_size_zero_extends() {
        assert_eq!(read_sized_be(&BYTES, 4, Size::Byte), Ok(0x9A));
        assert_eq!(read_sized_be(&BYTES, 4, Size::Word), Ok(0x9ABC));
        assert_eq!(read_sized_be(&BYTES, 0, Size::Long), Ok(0x1234_5678));
    }
}
