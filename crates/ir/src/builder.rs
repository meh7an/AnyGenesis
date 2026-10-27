use anygenesis_shared::Size;

use crate::{
    eval,
    op::{
        BinaryOp, Body, Control, ConvertOp, Expr, Flag, FlagOp, Op, Reg, ShiftOp, UnaryOp, Value,
        Width, result_width,
    },
};

/// Appends ops to a [`Body`], allocating one temporary per expression.
#[derive(Debug, Default)]
pub(crate) struct Builder {
    body: Body,
}

impl Builder {
    pub(crate) fn finish(self) -> Body {
        self.body
    }

    pub(crate) fn width(&self, value: Value) -> Width {
        self.body.width(value)
    }

    pub(crate) fn op(&mut self, op: Op) {
        self.body.ops.push(op);
    }

    /// Assigns `expr` to a new temporary and returns it, or returns the value directly when
    /// it folds: every operand constant, a constant select condition, or an identity
    /// (`x + 0`, `x & all ones`, `true & x`, ...).
    pub(crate) fn emit(&mut self, expr: Expr) -> Value {
        let width = result_width(&expr, |value| self.body.width(value));
        if let Some(value) = self.fold(&expr, width) {
            return value;
        }
        // A 68000 instruction needs at most a few hundred temporaries (MOVEM of all 16
        // registers is the largest), far below u16::MAX.
        let dst = crate::op::Temp(self.body.temps.len() as u16);
        self.body.temps.push(width);
        self.body.ops.push(Op::Let { dst, expr });
        Value::Temp(dst)
    }

    fn fold(&self, expr: &Expr, width: Width) -> Option<Value> {
        let constant = |value: Value| match value {
            Value::Const { value, .. } => Some(value),
            Value::Temp(_) => None,
        };
        let folded = match *expr {
            Expr::Unary { op, value } => eval::unary(op, width, constant(value)?),
            Expr::Binary { op, left, right } => {
                let operand_width = self.width(left);
                match (constant(left), constant(right)) {
                    (Some(left), Some(right)) => eval::binary(op, operand_width, left, right),
                    (_, Some(right)) => return Self::identity(op, operand_width, left, right),
                    (Some(left_value), None) => {
                        return Self::identity(op, operand_width, right, left_value)
                            .filter(|_| op != BinaryOp::Sub);
                    }
                    (None, None) => return None,
                }
            }
            Expr::Shift { op, value, count } => match (constant(value), constant(count)?) {
                (_, 0) => return Some(value),
                (Some(value), count) => eval::shift(op, width, value, count),
                (None, _) => return None,
            },
            Expr::Convert { op, value, .. } => {
                eval::convert(op, self.width(value), width, constant(value)?)
            }
            Expr::Select {
                condition,
                if_true,
                if_false,
            } => {
                return Some(if constant(condition)? != 0 {
                    if_true
                } else {
                    if_false
                });
            }
            Expr::Flag {
                op,
                left,
                right,
                result,
            } => u32::from(eval::flag(
                op,
                self.width(left),
                constant(left)?,
                constant(right)?,
                constant(result)?,
            )),
            Expr::ReadReg { .. }
            | Expr::ReadFlag(_)
            | Expr::ReadSr
            | Expr::Supervisor
            | Expr::Load { .. } => return None,
        };
        Some(Self::constant(width, folded))
    }

    /// Folds `operand op constant` when the constant makes the operation an identity or
    /// fixes its result (`false & x`, `true | x`). Callers exclude `constant - operand`.
    fn identity(op: BinaryOp, width: Width, operand: Value, constant: u32) -> Option<Value> {
        let all_ones = width.mask();
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Or | BinaryOp::Xor if constant == 0 => {
                Some(operand)
            }
            BinaryOp::And if constant == all_ones => Some(operand),
            BinaryOp::And if constant == 0 => Some(Self::constant(width, 0)),
            BinaryOp::Or if constant == all_ones && width == Width::Bool => {
                Some(Self::constant(width, 1))
            }
            _ => None,
        }
    }

    pub(crate) fn constant(width: Width, value: u32) -> Value {
        Value::Const {
            width,
            value: value & width.mask(),
        }
    }

    /// A constant of `value`'s width.
    pub(crate) fn like(&self, value: Value, constant: u32) -> Value {
        Self::constant(self.width(value), constant)
    }

    pub(crate) fn boolean(value: bool) -> Value {
        Self::constant(Width::Bool, u32::from(value))
    }

    pub(crate) fn read_reg(&mut self, reg: Reg, size: Size) -> Value {
        self.emit(Expr::ReadReg { reg, size })
    }

    pub(crate) fn write_reg(&mut self, reg: Reg, size: Size, value: Value) {
        self.op(Op::WriteReg { reg, size, value });
    }

    pub(crate) fn read_flag(&mut self, flag: Flag) -> Value {
        self.emit(Expr::ReadFlag(flag))
    }

    pub(crate) fn set_flag(&mut self, flag: Flag, value: Value) {
        self.op(Op::SetFlag { flag, value });
    }

    pub(crate) fn load(&mut self, size: Size, address: Value) -> Value {
        self.emit(Expr::Load { size, address })
    }

    pub(crate) fn store(&mut self, size: Size, address: Value, value: Value) {
        self.op(Op::Store {
            size,
            address,
            value,
        });
    }

    pub(crate) fn unary(&mut self, op: UnaryOp, value: Value) -> Value {
        self.emit(Expr::Unary { op, value })
    }

    pub(crate) fn binary(&mut self, op: BinaryOp, left: Value, right: Value) -> Value {
        self.emit(Expr::Binary { op, left, right })
    }

    /// `op` with a constant right operand of the left operand's width.
    pub(crate) fn binary_const(&mut self, op: BinaryOp, left: Value, right: u32) -> Value {
        let right = self.like(left, right);
        self.binary(op, left, right)
    }

    pub(crate) fn shift(&mut self, op: ShiftOp, value: Value, count: Value) -> Value {
        self.emit(Expr::Shift { op, value, count })
    }

    pub(crate) fn shift_const(&mut self, op: ShiftOp, value: Value, count: u32) -> Value {
        self.shift(op, value, Self::constant(Width::Byte, count))
    }

    pub(crate) fn convert(&mut self, op: ConvertOp, width: Width, value: Value) -> Value {
        self.emit(Expr::Convert { op, width, value })
    }

    /// Sign-extends a sized value to `Long`; a `Long` value is returned as it is.
    pub(crate) fn sign_extend_long(&mut self, value: Value) -> Value {
        match self.width(value) {
            Width::Long => value,
            _ => self.convert(ConvertOp::SignExtend, Width::Long, value),
        }
    }

    pub(crate) fn select(&mut self, condition: Value, if_true: Value, if_false: Value) -> Value {
        self.emit(Expr::Select {
            condition,
            if_true,
            if_false,
        })
    }

    pub(crate) fn flag_check(
        &mut self,
        op: FlagOp,
        left: Value,
        right: Value,
        result: Value,
    ) -> Value {
        self.emit(Expr::Flag {
            op,
            left,
            right,
            result,
        })
    }

    pub(crate) fn not(&mut self, value: Value) -> Value {
        self.unary(UnaryOp::Not, value)
    }

    pub(crate) fn and(&mut self, left: Value, right: Value) -> Value {
        self.binary(BinaryOp::And, left, right)
    }

    pub(crate) fn or(&mut self, left: Value, right: Value) -> Value {
        self.binary(BinaryOp::Or, left, right)
    }

    pub(crate) fn xor(&mut self, left: Value, right: Value) -> Value {
        self.binary(BinaryOp::Xor, left, right)
    }

    /// Whether `value` is zero.
    pub(crate) fn is_zero(&mut self, value: Value) -> Value {
        self.binary_const(BinaryOp::Eq, value, 0)
    }

    /// Whether the top bit of `value` is set.
    pub(crate) fn is_negative(&mut self, value: Value) -> Value {
        self.binary_const(BinaryOp::LtS, value, 0)
    }

    /// Whether any bit of `mask` is set in `value`.
    pub(crate) fn any_bit(&mut self, value: Value, mask: u32) -> Value {
        let masked = self.binary_const(BinaryOp::And, value, mask);
        self.binary_const(BinaryOp::Ne, masked, 0)
    }

    /// Charges `value` data-dependent cycles; a constant 0 charges nothing.
    pub(crate) fn cycles(&mut self, value: Value) {
        if !matches!(value, Value::Const { value: 0, .. }) {
            self.op(Op::Cycles { value });
        }
    }

    /// `if_true` cycles when `condition` holds, else none.
    pub(crate) fn cycles_if(&mut self, condition: Value, if_true: u32) {
        let charge = Self::constant(Width::Byte, if_true);
        let none = Self::constant(Width::Byte, 0);
        let value = self.select(condition, charge, none);
        self.cycles(value);
    }

    pub(crate) fn branch(&mut self, condition: Value, control: Control) {
        self.op(Op::Branch { condition, control });
    }

    pub(crate) fn exit(&mut self, control: Control) {
        self.op(Op::Exit(control));
    }
}
