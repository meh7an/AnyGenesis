//! Flag bits and the arithmetic, logic, rotate and shift operations as pure functions.
//!
//! Each operation returns its result and the whole new F; X and Y are the undocumented
//! copies of result bits 3 and 5 unless a function says otherwise.

pub(crate) const CARRY: u8 = 0x01;
pub(crate) const SUBTRACT: u8 = 0x02;
pub(crate) const PARITY: u8 = 0x04;
pub(crate) const X: u8 = 0x08;
pub(crate) const HALF: u8 = 0x10;
pub(crate) const Y: u8 = 0x20;
pub(crate) const ZERO: u8 = 0x40;
pub(crate) const SIGN: u8 = 0x80;

pub(crate) const XY: u8 = X | Y;

/// S, Z, X and Y of a result.
pub(crate) fn sz53(value: u8) -> u8 {
    let zero = if value == 0 { ZERO } else { 0 };
    (value & (SIGN | XY)) | zero
}

/// The parity flag of a value: set when it has an even number of 1 bits.
pub(crate) fn parity(value: u8) -> u8 {
    if value.count_ones().is_multiple_of(2) {
        PARITY
    } else {
        0
    }
}

/// S, Z, X, Y and P of a result.
pub(crate) fn sz53p(value: u8) -> u8 {
    sz53(value) | parity(value)
}

/// The eight accumulator operations, in opcode order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AluOp {
    Add,
    Adc,
    Sub,
    Sbc,
    And,
    Xor,
    Or,
    Cp,
}

impl AluOp {
    pub(crate) fn from_bits(bits: u8) -> Self {
        [
            Self::Add,
            Self::Adc,
            Self::Sub,
            Self::Sbc,
            Self::And,
            Self::Xor,
            Self::Or,
            Self::Cp,
        ][usize::from(bits & 7)]
    }
}

/// Applies `op` to A and `value`; CP returns A unchanged.
pub(crate) fn alu(op: AluOp, a: u8, value: u8, flags: u8) -> (u8, u8) {
    let carry = flags & CARRY;
    match op {
        AluOp::Add => add8(a, value, 0),
        AluOp::Adc => add8(a, value, carry),
        AluOp::Sub => sub8(a, value, 0),
        AluOp::Sbc => sub8(a, value, carry),
        AluOp::And => {
            let result = a & value;
            (result, sz53p(result) | HALF)
        }
        AluOp::Xor => {
            let result = a ^ value;
            (result, sz53p(result))
        }
        AluOp::Or => {
            let result = a | value;
            (result, sz53p(result))
        }
        AluOp::Cp => {
            // CP takes X and Y from the operand, not from the discarded difference.
            let (_, flags) = sub8(a, value, 0);
            (a, (flags & !XY) | (value & XY))
        }
    }
}

fn add8(a: u8, value: u8, carry: u8) -> (u8, u8) {
    let wide = u16::from(a) + u16::from(value) + u16::from(carry);
    let result = wide as u8;
    let overflow = if (a ^ result) & (value ^ result) & 0x80 != 0 {
        PARITY
    } else {
        0
    };
    let flags = sz53(result) | ((a ^ value ^ result) & HALF) | overflow | (wide >> 8) as u8;
    (result, flags)
}

fn sub8(a: u8, value: u8, carry: u8) -> (u8, u8) {
    let wide = u16::from(a)
        .wrapping_sub(u16::from(value))
        .wrapping_sub(u16::from(carry));
    let result = wide as u8;
    let overflow = if (a ^ value) & (a ^ result) & 0x80 != 0 {
        PARITY
    } else {
        0
    };
    let borrow = (wide >> 8) as u8 & CARRY;
    let flags = sz53(result) | ((a ^ value ^ result) & HALF) | overflow | SUBTRACT | borrow;
    (result, flags)
}

pub(crate) fn inc8(value: u8, flags: u8) -> (u8, u8) {
    let result = value.wrapping_add(1);
    let half = if result & 0x0F == 0 { HALF } else { 0 };
    let overflow = if result == 0x80 { PARITY } else { 0 };
    (result, (flags & CARRY) | sz53(result) | half | overflow)
}

pub(crate) fn dec8(value: u8, flags: u8) -> (u8, u8) {
    let result = value.wrapping_sub(1);
    let half = if result & 0x0F == 0x0F { HALF } else { 0 };
    let overflow = if result == 0x7F { PARITY } else { 0 };
    (
        result,
        (flags & CARRY) | sz53(result) | half | overflow | SUBTRACT,
    )
}

/// ADD HL,rp: S, Z and P/V are kept; H and C come from bits 11 and 15.
pub(crate) fn add16(left: u16, right: u16, flags: u8) -> (u16, u8) {
    let wide = u32::from(left) + u32::from(right);
    let result = wide as u16;
    let high = (result >> 8) as u8;
    let half = ((left ^ right ^ result) >> 8) as u8 & HALF;
    let flags = (flags & (SIGN | ZERO | PARITY)) | (high & XY) | half | (wide >> 16) as u8;
    (result, flags)
}

pub(crate) fn adc16(left: u16, right: u16, flags: u8) -> (u16, u8) {
    let wide = u32::from(left) + u32::from(right) + u32::from(flags & CARRY);
    let result = wide as u16;
    let overflow = if (left ^ result) & (right ^ result) & 0x8000 != 0 {
        PARITY
    } else {
        0
    };
    (
        result,
        flags16(left, right, result, overflow) | (wide >> 16) as u8,
    )
}

pub(crate) fn sbc16(left: u16, right: u16, flags: u8) -> (u16, u8) {
    let wide = u32::from(left)
        .wrapping_sub(u32::from(right))
        .wrapping_sub(u32::from(flags & CARRY));
    let result = wide as u16;
    let overflow = if (left ^ right) & (left ^ result) & 0x8000 != 0 {
        PARITY
    } else {
        0
    };
    let borrow = (wide >> 16) as u8 & CARRY;
    (
        result,
        flags16(left, right, result, overflow) | SUBTRACT | borrow,
    )
}

/// S, Z, X, Y and H of a 16-bit ADC or SBC, with its overflow flag.
fn flags16(left: u16, right: u16, result: u16, overflow: u8) -> u8 {
    let high = (result >> 8) as u8;
    let zero = if result == 0 { ZERO } else { 0 };
    let half = ((left ^ right ^ result) >> 8) as u8 & HALF;
    (high & (SIGN | XY)) | zero | half | overflow
}

/// The four accumulator rotates (RLCA, RRCA, RLA, RRA by `kind` 0-3): S, Z and P/V kept.
pub(crate) fn rotate_a(kind: u8, a: u8, flags: u8) -> (u8, u8) {
    let (result, carry) = shift(kind, a, flags);
    (
        result,
        (flags & (SIGN | ZERO | PARITY)) | (result & XY) | carry,
    )
}

/// The CB-prefixed rotates and shifts (RLC, RRC, RL, RR, SLA, SRA, SLL, SRL by `kind`).
pub(crate) fn rotate(kind: u8, value: u8, flags: u8) -> (u8, u8) {
    let (result, carry) = shift(kind, value, flags);
    (result, sz53p(result) | carry)
}

/// Shifts `value` one bit; returns the result and the bit shifted out as the carry flag.
fn shift(kind: u8, value: u8, flags: u8) -> (u8, u8) {
    let carry_in = flags & CARRY;
    let high = value >> 7;
    let low = value & 1;
    match kind & 7 {
        0 => (value.rotate_left(1), high),
        1 => (value.rotate_right(1), low),
        2 => ((value << 1) | carry_in, high),
        3 => ((value >> 1) | (carry_in << 7), low),
        4 => (value << 1, high),
        5 => ((value >> 1) | (value & 0x80), low),
        6 => ((value << 1) | 1, high),
        _ => (value >> 1, low),
    }
}

/// BIT n: X and Y come from `xy_source`, which depends on the addressing mode.
pub(crate) fn bit(number: u8, value: u8, xy_source: u8, flags: u8) -> u8 {
    let tested = value & (1 << number);
    let zero = if tested == 0 { ZERO | PARITY } else { 0 };
    (flags & CARRY) | HALF | zero | (tested & SIGN) | (xy_source & XY)
}

pub(crate) fn daa(a: u8, flags: u8) -> (u8, u8) {
    let mut correction = 0;
    let mut carry = flags & CARRY;
    if flags & HALF != 0 || a & 0x0F > 9 {
        correction |= 0x06;
    }
    if carry != 0 || a > 0x99 {
        correction |= 0x60;
        carry = CARRY;
    }
    let result = if flags & SUBTRACT != 0 {
        a.wrapping_sub(correction)
    } else {
        a.wrapping_add(correction)
    };
    let half = (a ^ result) & HALF;
    (result, sz53p(result) | half | (flags & SUBTRACT) | carry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_signed_overflow_sets_parity() {
        let (result, flags) = alu(AluOp::Add, 0x7F, 0x01, 0);
        assert_eq!(result, 0x80);
        assert_eq!(flags, SIGN | HALF | PARITY);
    }

    #[test]
    fn test_sub_borrow_sets_carry_and_subtract() {
        let (result, flags) = alu(AluOp::Sub, 0x00, 0x01, 0);
        assert_eq!(result, 0xFF);
        assert_eq!(flags, SIGN | Y | HALF | X | SUBTRACT | CARRY);
    }

    #[test]
    fn test_cp_takes_xy_from_operand() {
        let (result, flags) = alu(AluOp::Cp, 0x00, 0x28, 0);
        assert_eq!(result, 0x00);
        assert_eq!(flags & XY, XY);
    }

    #[test]
    fn test_daa_after_bcd_addition_corrects_both_digits() {
        let (sum, flags) = alu(AluOp::Add, 0x19, 0x28, 0);
        assert_eq!(daa(sum, flags).0, 0x47);
        let (sum, flags) = alu(AluOp::Add, 0x99, 0x01, 0);
        let (result, flags) = daa(sum, flags);
        assert_eq!(result, 0x00);
        assert_eq!(flags & (ZERO | CARRY), ZERO | CARRY);
    }

    #[test]
    fn test_rotate_sll_shifts_in_one() {
        assert_eq!(rotate(6, 0x80, 0), (0x01, CARRY));
    }

    #[test]
    fn test_sbc16_equal_operands_give_zero() {
        let (result, flags) = sbc16(0x1234, 0x1234, 0);
        assert_eq!(result, 0);
        assert_eq!(flags, ZERO | SUBTRACT);
    }
}
