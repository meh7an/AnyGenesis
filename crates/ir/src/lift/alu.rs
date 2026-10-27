//! Arithmetic, logic, multiply and divide, CHK and BCD.

use anygenesis_m68k::{Ea, Operand};
use anygenesis_shared::Size;

use super::{LiftError, Lifter, operand::Place, vector};
use crate::{
    builder::Builder,
    op::{
        BinaryOp, Control, ConvertOp, Expr, Flag, FlagOp, Op, Reg, ShiftOp, UnaryOp, Value, Width,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Arithmetic {
    Add,
    Sub,
}

impl Arithmetic {
    const fn op(self) -> BinaryOp {
        match self {
            Self::Add => BinaryOp::Add,
            Self::Sub => BinaryOp::Sub,
        }
    }

    const fn carry(self) -> FlagOp {
        match self {
            Self::Add => FlagOp::AddCarry,
            Self::Sub => FlagOp::SubCarry,
        }
    }

    const fn overflow(self) -> FlagOp {
        match self {
            Self::Add => FlagOp::AddOverflow,
            Self::Sub => FlagOp::SubOverflow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Logic {
    And,
    Or,
    Eor,
}

impl Logic {
    const fn op(self) -> BinaryOp {
        match self {
            Self::And => BinaryOp::And,
            Self::Or => BinaryOp::Or,
            Self::Eor => BinaryOp::Xor,
        }
    }
}

impl Lifter<'_> {
    /// ADD, ADDI, ADDQ, ADDA and the SUB forms. An address register destination takes the
    /// whole register and leaves the flags alone.
    pub(super) fn add_sub(&mut self, kind: Arithmetic) -> Result<(), LiftError> {
        let size = self.size()?;
        if let Ea::AddressRegister(number) = self.ea(1)? {
            let source = self.source(0, size)?;
            let source = self.out.sign_extend_long(source);
            let reg = Reg::Address(number);
            let destination = self.out.read_reg(reg, Size::Long);
            let result = self.out.binary(kind.op(), destination, source);
            self.out.write_reg(reg, Size::Long, result);
            return Ok(());
        }
        let source = self.source(0, size)?;
        let place = self.place(1, size)?;
        let destination = self.read(place, size);
        let result = self.out.binary(kind.op(), destination, source);
        self.write(place, size, result)?;
        let carry = self.set_arithmetic_flags(kind, destination, source, result);
        self.out.set_flag(Flag::X, carry);
        Ok(())
    }

    /// Sets N, Z, V and C for `left` (+ or -) `right` = `result` and returns the carry; X is
    /// the caller's.
    fn set_arithmetic_flags(
        &mut self,
        kind: Arithmetic,
        left: Value,
        right: Value,
        result: Value,
    ) -> Value {
        self.set_nz(result);
        self.set_carry_overflow(kind, left, right, result)
    }

    /// Sets V and C and returns the carry.
    fn set_carry_overflow(
        &mut self,
        kind: Arithmetic,
        left: Value,
        right: Value,
        result: Value,
    ) -> Value {
        let overflow = self.out.flag_check(kind.overflow(), left, right, result);
        self.out.set_flag(Flag::V, overflow);
        let carry = self.out.flag_check(kind.carry(), left, right, result);
        self.out.set_flag(Flag::C, carry);
        carry
    }

    /// ADDX and SUBX: X joins the operation, and Z is only ever cleared, so that a
    /// multi-precision chain tests the whole value for zero.
    pub(super) fn extended(&mut self, kind: Arithmetic) -> Result<(), LiftError> {
        let size = self.size()?;
        let source = self.source(0, size)?;
        let place = self.place(1, size)?;
        let destination = self.read(place, size);
        let x = self.extend_flag(Flag::X, size);
        let partial = self.out.binary(kind.op(), destination, source);
        let result = self.out.binary(kind.op(), partial, x);
        self.write(place, size, result)?;
        self.set_extended_flags(kind, destination, source, result);
        Ok(())
    }

    /// NEG and NEGX: `0 - operand`, with X joining for NEGX.
    pub(super) fn negate(&mut self, extended: bool) -> Result<(), LiftError> {
        let size = self.size()?;
        let place = self.place(0, size)?;
        let operand = self.read(place, size);
        let zero = Builder::constant(size.into(), 0);
        let mut result = self.out.binary(BinaryOp::Sub, zero, operand);
        if extended {
            let x = self.extend_flag(Flag::X, size);
            result = self.out.binary(BinaryOp::Sub, result, x);
        }
        self.write(place, size, result)?;
        if extended {
            self.set_extended_flags(Arithmetic::Sub, zero, operand, result);
        } else {
            let carry = self.set_arithmetic_flags(Arithmetic::Sub, zero, operand, result);
            self.out.set_flag(Flag::X, carry);
        }
        Ok(())
    }

    /// The flags of ADDX, SUBX and NEGX: N, V, C and X as usual, Z cleared on a nonzero
    /// result and otherwise unchanged.
    fn set_extended_flags(&mut self, kind: Arithmetic, left: Value, right: Value, result: Value) {
        let negative = self.out.is_negative(result);
        self.out.set_flag(Flag::N, negative);
        self.keep_zero_unless_nonzero(result);
        let carry = self.set_carry_overflow(kind, left, right, result);
        self.out.set_flag(Flag::X, carry);
    }

    fn keep_zero_unless_nonzero(&mut self, result: Value) {
        let zero = self.out.read_flag(Flag::Z);
        let is_zero = self.out.is_zero(result);
        let still_zero = self.out.and(zero, is_zero);
        self.out.set_flag(Flag::Z, still_zero);
    }

    /// A flag as 0 or 1 at `size`.
    fn extend_flag(&mut self, flag: Flag, size: Size) -> Value {
        let value = self.out.read_flag(flag);
        self.out.convert(ConvertOp::ZeroExtend, size.into(), value)
    }

    /// CMP, CMPI, CMPM and CMPA (whose source is sign-extended to a long compare).
    pub(super) fn compare(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let source = self.source(0, size)?;
        let (destination, source) = match self.ea(1)? {
            Ea::AddressRegister(number) => {
                let source = self.out.sign_extend_long(source);
                let destination = self.out.read_reg(Reg::Address(number), Size::Long);
                (destination, source)
            }
            _ => {
                let place = self.place(1, size)?;
                (self.read(place, size), source)
            }
        };
        let result = self.out.binary(BinaryOp::Sub, destination, source);
        self.set_arithmetic_flags(Arithmetic::Sub, destination, source, result);
        Ok(())
    }

    /// AND, OR, EOR and their immediate forms, including the CCR and SR targets.
    pub(super) fn logic(&mut self, kind: Logic) -> Result<(), LiftError> {
        let size = self.size()?;
        match self.operand(1)? {
            Operand::Ccr => {
                let source = self.source(0, Size::Byte)?;
                let ccr = self.read(Place::Ccr, Size::Byte);
                let result = self.out.binary(kind.op(), ccr, source);
                self.out.op(Op::WriteCcr { value: result });
            }
            Operand::Sr => {
                self.require_supervisor();
                let source = self.source(0, Size::Word)?;
                let sr = self.read(Place::Sr, Size::Word);
                let result = self.out.binary(kind.op(), sr, source);
                self.out.op(Op::WriteSr { value: result });
            }
            _ => {
                let source = self.source(0, size)?;
                let place = self.place(1, size)?;
                let destination = self.read(place, size);
                let result = self.out.binary(kind.op(), destination, source);
                self.write(place, size, result)?;
                self.set_logic_flags(result);
            }
        }
        Ok(())
    }

    pub(super) fn not(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let place = self.place(0, size)?;
        let operand = self.read(place, size);
        let result = self.out.not(operand);
        self.write(place, size, result)?;
        self.set_logic_flags(result);
        Ok(())
    }

    /// CLR. The 68000 reads the operand before clearing it; that extra read is not modelled.
    pub(super) fn clear(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let place = self.place(0, size)?;
        let zero = Builder::constant(size.into(), 0);
        self.write(place, size, zero)?;
        self.set_logic_flags(zero);
        Ok(())
    }

    pub(super) fn test(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let operand = self.source(0, size)?;
        self.set_logic_flags(operand);
        Ok(())
    }

    /// TAS: flags from the byte, then bit 7 set.
    pub(super) fn test_and_set(&mut self) -> Result<(), LiftError> {
        let place = self.place(0, Size::Byte)?;
        let operand = self.read(place, Size::Byte);
        self.set_logic_flags(operand);
        let result = self.out.binary_const(BinaryOp::Or, operand, 0x80);
        self.write(place, Size::Byte, result)
    }

    /// EXT.w (byte to word) and EXT.l (word to long).
    pub(super) fn extend(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let reg = Reg::Data(self.data_register(0)?);
        let from = match size {
            Size::Word => Size::Byte,
            Size::Long => Size::Word,
            Size::Byte => return Err(self.malformed()),
        };
        let operand = self.out.read_reg(reg, from);
        let result = self
            .out
            .convert(ConvertOp::SignExtend, size.into(), operand);
        self.out.write_reg(reg, size, result);
        self.set_logic_flags(result);
        Ok(())
    }

    /// SWAP: exchanges the register halves; the flags test the whole long.
    pub(super) fn swap(&mut self) -> Result<(), LiftError> {
        let reg = Reg::Data(self.data_register(0)?);
        let operand = self.out.read_reg(reg, Size::Long);
        let high = self.out.shift_const(ShiftOp::Shl, operand, 16);
        let low = self.out.shift_const(ShiftOp::Shr, operand, 16);
        let result = self.out.or(high, low);
        self.out.write_reg(reg, Size::Long, result);
        self.set_logic_flags(result);
        Ok(())
    }

    /// MULU and MULS: word by word into a long.
    pub(super) fn multiply(&mut self, signed: bool) -> Result<(), LiftError> {
        let source = self.source(0, Size::Word)?;
        let reg = Reg::Data(self.data_register(1)?);
        let destination = self.out.read_reg(reg, Size::Word);
        let extend = if signed {
            ConvertOp::SignExtend
        } else {
            ConvertOp::ZeroExtend
        };
        let left = self.out.convert(extend, Width::Long, destination);
        let right = self.out.convert(extend, Width::Long, source);
        let result = self.out.binary(BinaryOp::Mul, left, right);
        self.out.write_reg(reg, Size::Long, result);
        self.set_logic_flags(result);
        // Two cycles per set bit of the source for MULU, per 01 or 10 pair of its bits (a 0
        // below bit 0) for MULS (Table 8-9's 2n).
        let pattern = if signed {
            let moved = self.out.shift_const(ShiftOp::Shl, source, 1);
            self.out.xor(moved, source)
        } else {
            source
        };
        let ones = self.out.unary(UnaryOp::Ones, pattern);
        let cycles = self.out.binary(BinaryOp::Add, ones, ones);
        self.out.cycles(cycles);
        Ok(())
    }

    /// DIVU and DIVS: a long by a word into a word quotient (low) and remainder (high).
    ///
    /// Division by zero traps with the flags unchanged (no SingleStepTests case covers it;
    /// Mehran chose Musashi's behaviour). On overflow the register is kept and the flags
    /// are N=1, Z=0, V=1, C=0, as SingleStepTests shows for both instructions.
    pub(super) fn divide(&mut self, signed: bool) -> Result<(), LiftError> {
        let source = self.source(0, Size::Word)?;
        let reg = Reg::Data(self.data_register(1)?);
        let dividend = self.out.read_reg(reg, Size::Long);
        let by_zero = self.out.is_zero(source);
        self.out.branch(
            by_zero,
            Control::Exception {
                vector: vector::ZERO_DIVIDE,
                pc: self.next(),
            },
        );
        let (extend, divide, remainder, time) = if signed {
            (
                ConvertOp::SignExtend,
                BinaryOp::DivS,
                BinaryOp::RemS,
                BinaryOp::DivsTime,
            )
        } else {
            (
                ConvertOp::ZeroExtend,
                BinaryOp::DivU,
                BinaryOp::RemU,
                BinaryOp::DivuTime,
            )
        };
        let divisor = self.out.convert(extend, Width::Long, source);
        let quotient = self.out.binary(divide, dividend, divisor);
        let rest = self.out.binary(remainder, dividend, divisor);
        let low = self.out.convert(ConvertOp::Truncate, Width::Word, quotient);
        let overflow = if signed {
            let back = self.out.convert(ConvertOp::SignExtend, Width::Long, low);
            self.out.binary(BinaryOp::Ne, back, quotient)
        } else {
            let limit = Builder::constant(Width::Long, 0xFFFF);
            self.out.binary(BinaryOp::LtU, limit, quotient)
        };
        let high = self.out.shift_const(ShiftOp::Shl, rest, 16);
        let low_long = self.out.convert(ConvertOp::ZeroExtend, Width::Long, low);
        let packed = self.out.or(high, low_long);
        let result = self.out.select(overflow, dividend, packed);
        self.out.write_reg(reg, Size::Long, result);
        let negative = self.out.is_negative(low);
        let negative = self.out.or(overflow, negative);
        self.out.set_flag(Flag::N, negative);
        let fits = self.out.not(overflow);
        let zero = self.out.is_zero(low);
        let zero = self.out.and(fits, zero);
        self.out.set_flag(Flag::Z, zero);
        self.out.set_flag(Flag::V, overflow);
        self.out.set_flag(Flag::C, Builder::boolean(false));
        let cycles = self.out.binary(time, dividend, divisor);
        self.out.cycles(cycles);
        Ok(())
    }

    /// CHK: traps when Dn.w is negative or above the bound. The flags the manual leaves
    /// undefined follow SingleStepTests, trap or not: N = Dn < 0, Z = Dn == 0, V = C = 0.
    pub(super) fn check(&mut self) -> Result<(), LiftError> {
        let bound = self.source(0, Size::Word)?;
        let reg = Reg::Data(self.data_register(1)?);
        let value = self.out.read_reg(reg, Size::Word);
        let negative = self.out.is_negative(value);
        self.out.set_flag(Flag::N, negative);
        let zero = self.out.is_zero(value);
        self.out.set_flag(Flag::Z, zero);
        self.out.set_flag(Flag::V, Builder::boolean(false));
        self.out.set_flag(Flag::C, Builder::boolean(false));
        let above = self.out.binary(BinaryOp::LtS, bound, value);
        let out_of_bounds = self.out.or(negative, above);
        // The trap takes 2 cycles more when Dn is negative and bound - Dn has bit 15 clear
        // (fitted to SingleStepTests); without a trap Dn is never negative.
        let difference = self.out.binary(BinaryOp::Sub, bound, value);
        let difference_negative = self.out.is_negative(difference);
        let difference_positive = self.out.not(difference_negative);
        let slow = self.out.and(negative, difference_positive);
        self.out.cycles_if(slow, 2);
        self.out.branch(
            out_of_bounds,
            Control::Exception {
                vector: vector::CHK,
                pc: self.next(),
            },
        );
        Ok(())
    }

    /// ABCD and SBCD (`Dy,Dx` or `-(Ay),-(Ax)`).
    pub(super) fn bcd(&mut self, kind: Arithmetic) -> Result<(), LiftError> {
        let source = self.source(0, Size::Byte)?;
        let place = self.place(1, Size::Byte)?;
        let destination = self.read(place, Size::Byte);
        let result = self.bcd_operation(kind, destination, source);
        self.write(place, Size::Byte, result)
    }

    /// NBCD: `0 - operand - X` in BCD.
    pub(super) fn negate_bcd(&mut self) -> Result<(), LiftError> {
        let place = self.place(0, Size::Byte)?;
        let operand = self.read(place, Size::Byte);
        let zero = Builder::constant(Width::Byte, 0);
        let result = self.bcd_operation(Arithmetic::Sub, zero, operand);
        self.write(place, Size::Byte, result)
    }

    /// Computes `left (+ or -) right (+ or -) X` in BCD, sets the flags and returns the
    /// byte result.
    ///
    /// The formulas reproduce the 68000's binary operation followed by its decimal
    /// correction, including what invalid digits and the undefined V flag do; they matched
    /// every register-mode SingleStepTests case of ABCD, SBCD and NBCD (fit in M2.1, see
    /// PROGRESS.md). They work on words because the binary result needs nine bits.
    fn bcd_operation(&mut self, kind: Arithmetic, left: Value, right: Value) -> Value {
        let l = self.out.convert(ConvertOp::ZeroExtend, Width::Word, left);
        let r = self.out.convert(ConvertOp::ZeroExtend, Width::Word, right);
        let x = self.extend_flag(Flag::X, Size::Word);
        let partial = self.out.binary(kind.op(), l, r);
        let binary = self.out.binary(kind.op(), partial, x);
        let not_binary = self.out.not(binary);
        let (corrected, carry_bits, overflow_bits) = match kind {
            Arithmetic::Add => {
                // Carries out of each digit: bc from the binary sum, dc where a digit
                // exceeds 9.
                let both = self.out.and(l, r);
                let either = self.out.or(l, r);
                let lost = self.out.and(not_binary, either);
                let carries = self.out.or(both, lost);
                let bc = self.out.binary_const(BinaryOp::And, carries, 0x88);
                let plus_six = self.out.binary_const(BinaryOp::Add, binary, 0x66);
                let changed = self.out.xor(plus_six, binary);
                let decimal = self.out.binary_const(BinaryOp::And, changed, 0x110);
                let dc = self.out.shift_const(ShiftOp::Shr, decimal, 1);
                let digits = self.out.or(bc, dc);
                let correction = self.digit_correction(digits);
                let corrected = self.out.binary(BinaryOp::Add, binary, correction);
                let not_corrected = self.out.not(corrected);
                let wrapped = self.out.and(binary, not_corrected);
                let carry_bits = self.out.or(bc, wrapped);
                let overflow_bits = self.out.and(not_binary, corrected);
                (corrected, carry_bits, overflow_bits)
            }
            Arithmetic::Sub => {
                let not_l = self.out.not(l);
                let borrowed = self.out.and(not_l, r);
                let differ = self.out.xor(l, r);
                let same = self.out.not(differ);
                let kept = self.out.and(binary, same);
                let borrows = self.out.or(borrowed, kept);
                let bc = self.out.binary_const(BinaryOp::And, borrows, 0x88);
                let correction = self.digit_correction(bc);
                let corrected = self.out.binary(BinaryOp::Sub, binary, correction);
                let not_corrected = self.out.not(corrected);
                let wrapped = self.out.and(not_binary, corrected);
                let carry_bits = self.out.or(bc, wrapped);
                let overflow_bits = self.out.and(binary, not_corrected);
                (corrected, carry_bits, overflow_bits)
            }
        };
        let result = self
            .out
            .convert(ConvertOp::Truncate, Width::Byte, corrected);
        let negative = self.out.is_negative(result);
        self.out.set_flag(Flag::N, negative);
        self.keep_zero_unless_nonzero(result);
        let overflow = self.out.any_bit(overflow_bits, 0x80);
        self.out.set_flag(Flag::V, overflow);
        let carry = self.out.any_bit(carry_bits, 0x80);
        self.out.set_flag(Flag::C, carry);
        self.out.set_flag(Flag::X, carry);
        result
    }

    /// Turns digit carry bits (0x8 and 0x80) into the correction to apply: 6 per digit.
    fn digit_correction(&mut self, digits: Value) -> Value {
        let quarter = self.out.shift_const(ShiftOp::Shr, digits, 2);
        self.out.binary(BinaryOp::Sub, digits, quarter)
    }

    /// Exits through a privilege violation when the CPU is in user mode.
    pub(super) fn require_supervisor(&mut self) {
        let supervisor = self.out.emit(Expr::Supervisor);
        let user = self.out.not(supervisor);
        self.out.branch(
            user,
            Control::Exception {
                vector: vector::PRIVILEGE,
                pc: self.address,
            },
        );
    }
}
