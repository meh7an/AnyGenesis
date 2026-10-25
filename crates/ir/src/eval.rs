//! The Rust definition of the IR's pure primitives, on raw bits held in `u32`.
//!
//! The lifter folds constants with these, and the M2.2 interpreter evaluates with them, so
//! Rust has one definition of each primitive; the C emitter's helpers must match it. Every
//! function is total (see [`BinaryOp`] and [`ShiftOp`]). Inputs are expected to fit their
//! width; results always do. `Bool` values are 0 or 1.

use crate::op::{BinaryOp, ConvertOp, FlagOp, ShiftOp, UnaryOp, Width};

/// Sign-extends a value of `width` to 64 bits.
fn signed(width: Width, value: u32) -> i64 {
    let shift = 64 - width.bits();
    ((u64::from(value) << shift) as i64) >> shift
}

/// Returns whether the top bit of `value` at `width` is set.
fn top_bit(width: Width, value: u32) -> bool {
    value >> (width.bits() - 1) & 1 != 0
}

/// Evaluates a [`UnaryOp`] at `width`.
pub fn unary(op: UnaryOp, width: Width, value: u32) -> u32 {
    let result = match op {
        UnaryOp::Not => !value,
        UnaryOp::Neg => value.wrapping_neg(),
    };
    result & width.mask()
}

/// Evaluates a [`BinaryOp`] on operands of `width`; comparisons yield 0 or 1.
pub fn binary(op: BinaryOp, width: Width, left: u32, right: u32) -> u32 {
    let result = match op {
        BinaryOp::Add => left.wrapping_add(right),
        BinaryOp::Sub => left.wrapping_sub(right),
        BinaryOp::And => left & right,
        BinaryOp::Or => left | right,
        BinaryOp::Xor => left ^ right,
        BinaryOp::Mul => left.wrapping_mul(right),
        BinaryOp::DivU => left.checked_div(right).unwrap_or(0),
        BinaryOp::RemU => left.checked_rem(right).unwrap_or(0),
        // In 64 bits the most negative value divided by -1 cannot overflow; masking turns
        // the quotient back into that value.
        BinaryOp::DivS => signed(width, left)
            .checked_div(signed(width, right))
            .unwrap_or(0) as u32,
        BinaryOp::RemS => signed(width, left)
            .checked_rem(signed(width, right))
            .unwrap_or(0) as u32,
        BinaryOp::Eq => u32::from(left == right),
        BinaryOp::Ne => u32::from(left != right),
        BinaryOp::LtU => u32::from(left < right),
        BinaryOp::LtS => u32::from(signed(width, left) < signed(width, right)),
    };
    if op.is_comparison() {
        result
    } else {
        result & width.mask()
    }
}

/// Evaluates a [`ShiftOp`] on a value of `width` by `count`, any count.
pub fn shift(op: ShiftOp, width: Width, value: u32, count: u32) -> u32 {
    let result = if count >= width.bits() {
        match op {
            ShiftOp::Shl | ShiftOp::Shr => 0,
            ShiftOp::Sar if top_bit(width, value) => u32::MAX,
            ShiftOp::Sar => 0,
        }
    } else {
        match op {
            ShiftOp::Shl => value << count,
            ShiftOp::Shr => value >> count,
            ShiftOp::Sar => (signed(width, value) >> count) as u32,
        }
    };
    result & width.mask()
}

/// Converts `value` from width `from` to width `to`.
pub fn convert(op: ConvertOp, from: Width, to: Width, value: u32) -> u32 {
    let result = match op {
        ConvertOp::ZeroExtend | ConvertOp::Truncate => value,
        ConvertOp::SignExtend => signed(from, value) as u32,
    };
    result & to.mask()
}

/// Evaluates a [`FlagOp`] on two operands and a result, all of `width`.
pub fn flag(op: FlagOp, width: Width, left: u32, right: u32, result: u32) -> bool {
    let bits = match op {
        FlagOp::AddCarry => (left & right) | (!result & (left | right)),
        FlagOp::AddOverflow => (left ^ result) & (right ^ result),
        FlagOp::SubCarry => (!left & right) | (result & !(left ^ right)),
        FlagOp::SubOverflow => (left ^ right) & (left ^ result),
    };
    top_bit(width, bits & width.mask())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shift_count_past_width_is_defined() {
        assert_eq!(shift(ShiftOp::Shl, Width::Byte, 0xFF, 8), 0);
        assert_eq!(shift(ShiftOp::Shr, Width::Long, 0xFFFF_FFFF, 63), 0);
        assert_eq!(shift(ShiftOp::Sar, Width::Word, 0x8000, 40), 0xFFFF);
        assert_eq!(shift(ShiftOp::Sar, Width::Word, 0x7FFF, 255), 0);
        assert_eq!(shift(ShiftOp::Sar, Width::Byte, 0x80, 3), 0xF0);
    }

    #[test]
    fn test_division_edges_are_total() {
        assert_eq!(binary(BinaryOp::DivU, Width::Long, 7, 0), 0);
        assert_eq!(binary(BinaryOp::RemS, Width::Long, 7, 0), 0);
        let minimum = 0x8000_0000;
        assert_eq!(
            binary(BinaryOp::DivS, Width::Long, minimum, u32::MAX),
            minimum
        );
        assert_eq!(binary(BinaryOp::RemS, Width::Long, minimum, u32::MAX), 0);
        // Truncation toward zero, remainder with the dividend's sign: -7 / 2 = -3 rem -1.
        assert_eq!(
            binary(BinaryOp::DivS, Width::Long, (-7i32) as u32, 2),
            (-3i32) as u32
        );
        assert_eq!(
            binary(BinaryOp::RemS, Width::Long, (-7i32) as u32, 2),
            u32::MAX
        );
    }

    #[test]
    fn test_flags_of_byte_add_and_sub() {
        // 0x7F + 1 = 0x80: overflow, no carry. 0xFF + 1 = 0x00: carry, no overflow.
        assert!(flag(FlagOp::AddOverflow, Width::Byte, 0x7F, 0x01, 0x80));
        assert!(!flag(FlagOp::AddCarry, Width::Byte, 0x7F, 0x01, 0x80));
        assert!(flag(FlagOp::AddCarry, Width::Byte, 0xFF, 0x01, 0x00));
        assert!(!flag(FlagOp::AddOverflow, Width::Byte, 0xFF, 0x01, 0x00));
        // 0x00 - 1 = 0xFF: borrow. 0x80 - 1 = 0x7F: overflow.
        assert!(flag(FlagOp::SubCarry, Width::Byte, 0x00, 0x01, 0xFF));
        assert!(flag(FlagOp::SubOverflow, Width::Byte, 0x80, 0x01, 0x7F));
        assert!(!flag(FlagOp::SubCarry, Width::Byte, 0x80, 0x01, 0x7F));
    }

    #[test]
    fn test_flags_with_carry_in_use_the_result() {
        // ADDX 0xFF + 0x00 + X=1 = 0x00 carries; SUBX 0x00 - 0x00 - X=1 = 0xFF borrows.
        assert!(flag(FlagOp::AddCarry, Width::Byte, 0xFF, 0x00, 0x00));
        assert!(flag(FlagOp::SubCarry, Width::Byte, 0x00, 0x00, 0xFF));
    }

    #[test]
    fn test_convert_sign_extends_and_truncates() {
        assert_eq!(
            convert(ConvertOp::SignExtend, Width::Word, Width::Long, 0x8000),
            0xFFFF_8000
        );
        assert_eq!(
            convert(ConvertOp::Truncate, Width::Long, Width::Byte, 0x1234),
            0x34
        );
        assert_eq!(
            convert(ConvertOp::ZeroExtend, Width::Bool, Width::Word, 1),
            1
        );
    }
}
