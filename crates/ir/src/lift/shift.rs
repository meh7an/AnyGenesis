//! Shifts, rotates and bit operations.
//!
//! Counts from a register are taken mod 64, and every count from 0 to 63 is defined: the
//! IR's shifts give 0 (or sign fill) for counts of the width or more, which is what a
//! 68000 shifting one bit at a time produces. A count of 0 leaves the operand alone, clears
//! C (for rotates through X it copies X) and keeps X.

use anygenesis_m68k::{Ea, Operand};
use anygenesis_shared::Size;

use super::{LiftError, Lifter};
use crate::{
    builder::Builder,
    op::{BinaryOp, ConvertOp, Flag, Reg, ShiftOp, Value, Width},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Asl,
    Asr,
    Lsl,
    Lsr,
    Rol,
    Ror,
    Roxl,
    Roxr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BitOp {
    Test,
    Change,
    Clear,
    Set,
}

/// A shift result and the last bit shifted out, before the count-0 case is applied.
struct Shifted {
    result: Value,
    out: Value,
}

impl Lifter<'_> {
    /// Register forms (`#q,Dy` or `Dx,Dy`) and the one-bit memory form (`<ea>`, word).
    pub(super) fn shift(&mut self, kind: Kind) -> Result<(), LiftError> {
        let size = self.size()?;
        let (count, place, register_count) = match self.instruction.operands {
            [Some(Operand::Ea(Ea::DataRegister(number))), Some(_)] => {
                let count = self.out.read_reg(Reg::Data(number), Size::Byte);
                let count = self.out.binary_const(BinaryOp::And, count, 63);
                (count, self.place(1, size)?, true)
            }
            [Some(Operand::Quick(quick)), Some(_)] => (
                Builder::constant(Width::Byte, quick as u32),
                self.place(1, size)?,
                false,
            ),
            [Some(Operand::Ea(_)), None] => (
                Builder::constant(Width::Byte, 1),
                self.place(0, size)?,
                false,
            ),
            _ => return Err(self.malformed()),
        };
        self.shift_place(kind, size, place, count, register_count)?;
        if register_count {
            // Two cycles per step of a register count (Table 8-7); quick counts are static.
            let cycles = self.out.binary(BinaryOp::Add, count, count);
            self.out.cycles(cycles);
        }
        Ok(())
    }

    /// Shifts or rotates the operand at `place` by `count` and sets the flags.
    fn shift_place(
        &mut self,
        kind: Kind,
        size: Size,
        place: super::operand::Place,
        count: Value,
        register_count: bool,
    ) -> Result<(), LiftError> {
        let value = self.read(place, size);
        let width = Width::from(size).bits();
        let shifted = match kind {
            Kind::Asl | Kind::Lsl => self.shift_left(value, count, width),
            Kind::Asr => self.shift_right(ShiftOp::Sar, value, count),
            Kind::Lsr => self.shift_right(ShiftOp::Shr, value, count),
            Kind::Rol => self.rotate_left(value, count, width),
            Kind::Ror => self.rotate_right(value, count, width),
            Kind::Roxl => return self.rotate_extended(place, size, value, count, true),
            Kind::Roxr => return self.rotate_extended(place, size, value, count, false),
        };
        self.write(place, size, shifted.result)?;
        self.set_nz(shifted.result);
        let overflow = match kind {
            Kind::Asl => self.shift_left_overflow(value, count),
            _ => Builder::boolean(false),
        };
        self.out.set_flag(Flag::V, overflow);
        // Quick counts are 1 to 8, so only a register count can be 0.
        let carry = if register_count {
            let nonzero = self.out.binary_const(BinaryOp::Ne, count, 0);
            self.out.and(nonzero, shifted.out)
        } else {
            shifted.out
        };
        self.out.set_flag(Flag::C, carry);
        if !matches!(kind, Kind::Rol | Kind::Ror) {
            let x = if register_count {
                let nonzero = self.out.binary_const(BinaryOp::Ne, count, 0);
                let old = self.out.read_flag(Flag::X);
                self.out.select(nonzero, shifted.out, old)
            } else {
                shifted.out
            };
            self.out.set_flag(Flag::X, x);
        }
        Ok(())
    }

    /// The bit at position `bit` (a `Byte`; past the top it reads as 0).
    fn bit_at(&mut self, value: Value, bit: Value) -> Value {
        let moved = self.out.shift(ShiftOp::Shr, value, bit);
        self.out.any_bit(moved, 1)
    }

    /// `count` (a `Byte`) subtracted from `constant`, wrapping: a negative difference wraps
    /// past every width, so a shift by it gives 0.
    fn byte_difference(&mut self, constant: u32, count: Value) -> Value {
        let constant = Builder::constant(Width::Byte, constant);
        self.out.binary(BinaryOp::Sub, constant, count)
    }

    /// LSL and ASL: the last bit out is bit `width - count`.
    fn shift_left(&mut self, value: Value, count: Value, width: u32) -> Shifted {
        let result = self.out.shift(ShiftOp::Shl, value, count);
        let position = self.byte_difference(width, count);
        let out = self.bit_at(value, position);
        Shifted { result, out }
    }

    /// LSR and ASR: the last bit out is bit `count - 1` (of the sign-filled value for ASR).
    fn shift_right(&mut self, op: ShiftOp, value: Value, count: Value) -> Shifted {
        let result = self.out.shift(op, value, count);
        let one = Builder::constant(Width::Byte, 1);
        let position = self.out.binary(BinaryOp::Sub, count, one);
        let before = self.out.shift(op, value, position);
        let out = self.out.any_bit(before, 1);
        Shifted { result, out }
    }

    /// ASL's V: set when the sign bit changes at any step, that is, when shifting back
    /// arithmetically does not restore the operand (for counts past the width: when the
    /// operand is nonzero).
    fn shift_left_overflow(&mut self, value: Value, count: Value) -> Value {
        let shifted = self.out.shift(ShiftOp::Shl, value, count);
        let back = self.out.shift(ShiftOp::Sar, shifted, count);
        self.out.binary(BinaryOp::Ne, back, value)
    }

    /// ROL: rotates by `count mod width`; C is the result's bit 0.
    fn rotate_left(&mut self, value: Value, count: Value, width: u32) -> Shifted {
        let amount = self.out.binary_const(BinaryOp::And, count, width - 1);
        let high = self.out.shift(ShiftOp::Shl, value, amount);
        let back = self.byte_difference(width, amount);
        let low = self.out.shift(ShiftOp::Shr, value, back);
        let result = self.out.or(high, low);
        let out = self.out.any_bit(result, 1);
        Shifted { result, out }
    }

    /// ROR: rotates by `count mod width`; C is the result's top bit.
    fn rotate_right(&mut self, value: Value, count: Value, width: u32) -> Shifted {
        let amount = self.out.binary_const(BinaryOp::And, count, width - 1);
        let low = self.out.shift(ShiftOp::Shr, value, amount);
        let back = self.byte_difference(width, amount);
        let high = self.out.shift(ShiftOp::Shl, value, back);
        let result = self.out.or(low, high);
        let out = self.out.is_negative(result);
        Shifted { result, out }
    }

    /// ROXL and ROXR: rotates through X, a ring of `width + 1` bits, by `count mod
    /// (width + 1)`. A rotation by 0 (any multiple of the ring) leaves the operand and X
    /// and copies X into C. Writes the result and sets every flag.
    fn rotate_extended(
        &mut self,
        place: super::operand::Place,
        size: Size,
        value: Value,
        count: Value,
        left: bool,
    ) -> Result<(), LiftError> {
        let width = Width::from(size).bits();
        let amount = self.out.binary_const(BinaryOp::RemU, count, width + 1);
        let old_x = self.out.read_flag(Flag::X);
        let x = self.out.convert(ConvertOp::ZeroExtend, size.into(), old_x);
        let one = Builder::constant(Width::Byte, 1);
        let amount_less_one = self.out.binary(BinaryOp::Sub, amount, one);
        let wrap = self.byte_difference(width + 1, amount);
        // For ROXL: value << k | X << (k - 1) | value >> (width + 1 - k); the last bit out
        // is bit width - k. ROXR mirrors it. With k = 0 the shifts by k - 1 (wrapped) and by
        // width + 1 give 0, so the result is the operand.
        let (result, out) = if left {
            let main = self.out.shift(ShiftOp::Shl, value, amount);
            let inserted = self.out.shift(ShiftOp::Shl, x, amount_less_one);
            let wrapped = self.out.shift(ShiftOp::Shr, value, wrap);
            let partial = self.out.or(main, inserted);
            let result = self.out.or(partial, wrapped);
            let position = self.byte_difference(width, amount);
            (result, self.bit_at(value, position))
        } else {
            let main = self.out.shift(ShiftOp::Shr, value, amount);
            let position = self.byte_difference(width, amount);
            let inserted = self.out.shift(ShiftOp::Shl, x, position);
            let wrapped = self.out.shift(ShiftOp::Shl, value, wrap);
            let partial = self.out.or(main, inserted);
            let result = self.out.or(partial, wrapped);
            (result, self.bit_at(value, amount_less_one))
        };
        self.write(place, size, result)?;
        self.set_nz(result);
        self.out.set_flag(Flag::V, Builder::boolean(false));
        let rotated = self.out.binary_const(BinaryOp::Ne, amount, 0);
        let carry = self.out.select(rotated, out, old_x);
        self.out.set_flag(Flag::C, carry);
        self.out.set_flag(Flag::X, carry);
        Ok(())
    }

    /// BTST, BCHG, BCLR and BSET: bit number mod 32 on a data register, mod 8 in memory. Z is
    /// the complement of the tested bit; the other flags are unaffected.
    pub(super) fn bit(&mut self, op: BitOp) -> Result<(), LiftError> {
        let size = self.size()?;
        let modulus = Width::from(size).bits() - 1;
        let number = match self.ea(0)? {
            Ea::DataRegister(number) => {
                let number = self.out.read_reg(Reg::Data(number), Size::Byte);
                self.out.binary_const(BinaryOp::And, number, modulus)
            }
            Ea::Immediate(value) => Builder::constant(Width::Byte, value & modulus),
            _ => return Err(self.malformed()),
        };
        let place = self.place(1, size)?;
        let value = self.read(place, size);
        let one = Builder::constant(size.into(), 1);
        let mask = self.out.shift(ShiftOp::Shl, one, number);
        let tested = self.out.and(value, mask);
        let clear = self.out.is_zero(tested);
        self.out.set_flag(Flag::Z, clear);
        let result = match op {
            BitOp::Test => return Ok(()),
            BitOp::Change => self.out.xor(value, mask),
            BitOp::Clear => {
                let keep = self.out.not(mask);
                self.out.and(value, keep)
            }
            BitOp::Set => self.out.or(value, mask),
        };
        self.write(place, size, result)?;
        // Changing a bit of a data register's upper word costs 2 cycles more; for a constant
        // bit number the static time holds it.
        if size == Size::Long && matches!(number, Value::Temp(_)) {
            let fifteen = Builder::constant(Width::Byte, 15);
            let upper = self.out.binary(BinaryOp::LtU, fifteen, number);
            self.out.cycles_if(upper, 2);
        }
        Ok(())
    }
}
