/// A 68000 operand size.
///
/// Not `#[non_exhaustive]`: the 68000 has exactly these three sizes, and every match on them
/// in the decoder, IR and runtime must stay exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    /// 8 bits, `.b`.
    Byte,
    /// 16 bits, `.w`.
    Word,
    /// 32 bits, `.l`.
    Long,
}

impl Size {
    /// Returns the operand width in bytes.
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Long => 4,
        }
    }

    /// Returns the operand width in bits.
    pub const fn bits(self) -> u32 {
        self.bytes() * 8
    }

    /// Returns a mask covering the operand's bits.
    pub const fn mask(self) -> u32 {
        u32::MAX >> (32 - self.bits())
    }

    /// Returns the operand's sign bit.
    pub const fn sign_bit(self) -> u32 {
        1 << (self.bits() - 1)
    }

    /// Sign-extends the operand held in the low bits of `value` to 32 bits.
    pub const fn sign_extend(self, value: u32) -> i32 {
        let shift = 32 - self.bits();
        ((value << shift) as i32) >> shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_mask_each_size_covers_its_bits() {
        assert_eq!(Size::Byte.mask(), 0x0000_00FF);
        assert_eq!(Size::Word.mask(), 0x0000_FFFF);
        assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);
    }

    #[test]
    fn test_size_sign_bit_each_size_is_top_bit() {
        assert_eq!(Size::Byte.sign_bit(), 0x0000_0080);
        assert_eq!(Size::Word.sign_bit(), 0x0000_8000);
        assert_eq!(Size::Long.sign_bit(), 0x8000_0000);
    }

    #[test]
    fn test_size_sign_extend_negative_operand_fills_high_bits() {
        assert_eq!(Size::Byte.sign_extend(0x1234_56FF), -1);
        assert_eq!(Size::Byte.sign_extend(0x0000_007F), 127);
        assert_eq!(Size::Word.sign_extend(0x0000_8000), -32768);
        assert_eq!(Size::Long.sign_extend(0xFFFF_FFFE), -2);
    }
}
