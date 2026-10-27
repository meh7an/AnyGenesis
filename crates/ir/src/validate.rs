use crate::op::{
    BinaryOp, Body, Control, ConvertOp, Expr, Op, Reg, UnaryOp, Value, Width, result_width,
};

/// A structural problem in a [`Body`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("op {op}: {problem}")]
pub struct ValidationError {
    /// The index of the offending op.
    pub op: usize,
    pub problem: &'static str,
}

/// Checks that `body` is well formed:
/// - each `Let` defines the next temporary, with the width its expression yields, so every
///   temporary is assigned exactly once and before any use;
/// - operand widths fit each operation, constants fit their width;
/// - register numbers are 0 to 7, and address registers are written whole;
/// - no `Load` or `Store` follows an `Op::Cycles`;
/// - an `Exit` comes only last.
///
/// # Errors
/// Returns the first problem found.
pub fn validate(body: &Body) -> Result<(), ValidationError> {
    let mut checker = Checker {
        body,
        defined: 0,
        op: 0,
        charged: false,
    };
    for (index, op) in body.ops.iter().enumerate() {
        checker.op = index;
        checker.op(op, index + 1 == body.ops.len())?;
    }
    if checker.defined != body.temps.len() {
        return Err(ValidationError {
            op: body.ops.len(),
            problem: "temporaries declared but never defined",
        });
    }
    Ok(())
}

struct Checker<'a> {
    body: &'a Body,
    defined: usize,
    op: usize,
    /// Whether an `Op::Cycles` came before the current op.
    charged: bool,
}

impl Checker<'_> {
    fn fail(&self, problem: &'static str) -> ValidationError {
        ValidationError {
            op: self.op,
            problem,
        }
    }

    fn ensure(&self, condition: bool, problem: &'static str) -> Result<(), ValidationError> {
        if condition {
            Ok(())
        } else {
            Err(self.fail(problem))
        }
    }

    /// Checks that `value` is defined (or a fitting constant) and returns its width.
    fn value(&self, value: Value) -> Result<Width, ValidationError> {
        match value {
            Value::Temp(temp) => {
                self.ensure(
                    temp.index() < self.defined,
                    "temporary used before definition",
                )?;
                Ok(self.body.temps[temp.index()])
            }
            Value::Const { width, value } => {
                self.ensure(value & !width.mask() == 0, "constant wider than its width")?;
                Ok(width)
            }
        }
    }

    fn expect(
        &self,
        value: Value,
        width: Width,
        problem: &'static str,
    ) -> Result<(), ValidationError> {
        let actual = self.value(value)?;
        self.ensure(actual == width, problem)
    }

    fn sized(&self, value: Value) -> Result<Width, ValidationError> {
        let width = self.value(value)?;
        self.ensure(width != Width::Bool, "sized operand expected, found bool")?;
        Ok(width)
    }

    fn register(&self, reg: Reg) -> Result<(), ValidationError> {
        match reg {
            Reg::Data(number) | Reg::Address(number) => {
                self.ensure(number < 8, "register number above 7")
            }
            Reg::InactiveSp => Ok(()),
        }
    }

    fn op(&mut self, op: &Op, last: bool) -> Result<(), ValidationError> {
        let access = matches!(
            op,
            Op::Store { .. }
                | Op::Let {
                    expr: Expr::Load { .. },
                    ..
                }
        );
        self.ensure(!(access && self.charged), "bus access after a cycle charge")?;
        match *op {
            Op::Let { dst, expr } => {
                self.ensure(
                    dst.index() == self.defined,
                    "temporary defined out of order",
                )?;
                self.ensure(
                    dst.index() < self.body.temps.len(),
                    "temporary not declared",
                )?;
                self.expr(&expr)?;
                let width = result_width(&expr, |value| self.body.width(value));
                self.ensure(
                    self.body.temps[dst.index()] == width,
                    "temporary width mismatch",
                )?;
                self.defined += 1;
            }
            Op::WriteReg { reg, size, value } => {
                self.register(reg)?;
                if !matches!(reg, Reg::Data(_)) {
                    self.ensure(
                        size == anygenesis_shared::Size::Long,
                        "address register written partially",
                    )?;
                }
                self.expect(value, size.into(), "register write width mismatch")?;
            }
            Op::SetFlag { value, .. } => self.expect(value, Width::Bool, "flag value not bool")?,
            Op::WriteSr { value } => self.expect(value, Width::Word, "SR value not a word")?,
            Op::WriteCcr { value } => self.expect(value, Width::Byte, "CCR value not a byte")?,
            Op::Store {
                size,
                address,
                value,
            } => {
                self.expect(address, Width::Long, "address not a long")?;
                self.expect(value, size.into(), "store width mismatch")?;
            }
            Op::ResetDevices => {}
            Op::Cycles { value } => {
                self.sized(value)?;
                self.charged = true;
            }
            Op::Branch { condition, control } => {
                self.expect(condition, Width::Bool, "branch condition not bool")?;
                self.control(control)?;
            }
            Op::Exit(control) => {
                self.ensure(last, "exit before the last op")?;
                self.control(control)?;
            }
        }
        Ok(())
    }

    fn expr(&self, expr: &Expr) -> Result<(), ValidationError> {
        match *expr {
            Expr::ReadReg { reg, .. } => self.register(reg),
            Expr::ReadFlag(_) | Expr::ReadSr | Expr::Supervisor => Ok(()),
            Expr::Load { address, .. } => self.expect(address, Width::Long, "address not a long"),
            Expr::Unary { op, value } => {
                let width = self.value(value)?;
                self.ensure(
                    op == UnaryOp::Not || width != Width::Bool,
                    "arithmetic on a bool",
                )
            }
            Expr::Binary { op, left, right } => {
                let width = self.value(left)?;
                self.expect(right, width, "binary operand widths differ")?;
                let bool_allowed = matches!(
                    op,
                    BinaryOp::And | BinaryOp::Or | BinaryOp::Xor | BinaryOp::Eq | BinaryOp::Ne
                );
                self.ensure(bool_allowed || width != Width::Bool, "arithmetic on a bool")?;
                let timing = matches!(op, BinaryOp::DivuTime | BinaryOp::DivsTime);
                self.ensure(
                    !timing || width == Width::Long,
                    "division time not on longs",
                )
            }
            Expr::Shift { value, count, .. } => {
                self.sized(value)?;
                self.expect(count, Width::Byte, "shift count not a byte")
            }
            Expr::Convert { op, width, value } => {
                let from = self.value(value)?;
                let valid = match op {
                    ConvertOp::ZeroExtend => from.bits() < width.bits(),
                    ConvertOp::SignExtend => from != Width::Bool && from.bits() < width.bits(),
                    ConvertOp::Truncate => width != Width::Bool && width.bits() < from.bits(),
                };
                self.ensure(valid, "conversion does not change width the right way")
            }
            Expr::Select {
                condition,
                if_true,
                if_false,
            } => {
                self.expect(condition, Width::Bool, "select condition not bool")?;
                let width = self.value(if_true)?;
                self.expect(if_false, width, "select arm widths differ")
            }
            Expr::Flag {
                left,
                right,
                result,
                ..
            } => {
                let width = self.sized(left)?;
                self.expect(right, width, "flag operand widths differ")?;
                self.expect(result, width, "flag result width differs")
            }
        }
    }

    fn control(&self, control: Control) -> Result<(), ValidationError> {
        match control {
            Control::JumpIndirect(target)
            | Control::CallIndirect { target, .. }
            | Control::Return { target, .. } => {
                self.expect(target, Width::Long, "control target not a long")
            }
            Control::Jump(_)
            | Control::Call { .. }
            | Control::Exception { .. }
            | Control::Stop { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use anygenesis_shared::{Address, Size};

    use super::*;
    use crate::op::{Flag, Temp};

    fn long(value: u32) -> Value {
        Value::Const {
            width: Width::Long,
            value,
        }
    }

    fn read_d0() -> Op {
        Op::Let {
            dst: Temp(0),
            expr: Expr::ReadReg {
                reg: Reg::Data(0),
                size: Size::Long,
            },
        }
    }

    fn problem(body: &Body) -> &'static str {
        validate(body).map_or_else(|error| error.problem, |()| "valid")
    }

    #[test]
    fn test_validate_well_formed_body_is_valid() {
        let body = Body {
            temps: vec![Width::Long],
            ops: vec![
                read_d0(),
                Op::Exit(Control::JumpIndirect(Value::Temp(Temp(0)))),
            ],
        };
        assert_eq!(problem(&body), "valid");
    }

    #[test]
    fn test_validate_use_before_definition_is_rejected() {
        let body = Body {
            temps: vec![Width::Long],
            ops: vec![
                Op::WriteReg {
                    reg: Reg::Data(1),
                    size: Size::Long,
                    value: Value::Temp(Temp(0)),
                },
                read_d0(),
            ],
        };
        assert_eq!(problem(&body), "temporary used before definition");
    }

    #[test]
    fn test_validate_width_mismatch_is_rejected() {
        let body = Body {
            temps: vec![Width::Long],
            ops: vec![
                read_d0(),
                Op::SetFlag {
                    flag: Flag::Z,
                    value: Value::Temp(Temp(0)),
                },
            ],
        };
        assert_eq!(problem(&body), "flag value not bool");
    }

    #[test]
    fn test_validate_bus_access_after_cycle_charge_is_rejected() {
        let body = Body {
            temps: Vec::new(),
            ops: vec![
                Op::Cycles { value: long(2) },
                Op::Store {
                    size: Size::Long,
                    address: long(0xFF_0000),
                    value: long(0),
                },
            ],
        };
        assert_eq!(problem(&body), "bus access after a cycle charge");
    }

    #[test]
    fn test_validate_exit_before_last_op_is_rejected() {
        let body = Body {
            temps: vec![Width::Long],
            ops: vec![Op::Exit(Control::Jump(Address::new(0))), read_d0()],
        };
        assert_eq!(problem(&body), "exit before the last op");
    }

    #[test]
    fn test_validate_partial_address_register_write_is_rejected() {
        let body = Body {
            temps: Vec::new(),
            ops: vec![Op::WriteReg {
                reg: Reg::Address(0),
                size: Size::Word,
                value: Value::Const {
                    width: Width::Word,
                    value: 1,
                },
            }],
        };
        assert_eq!(problem(&body), "address register written partially");
    }

    #[test]
    fn test_validate_oversized_constant_is_rejected() {
        let body = Body {
            temps: Vec::new(),
            ops: vec![Op::Store {
                size: Size::Byte,
                address: long(0xFF_0000),
                value: Value::Const {
                    width: Width::Byte,
                    value: 0x100,
                },
            }],
        };
        assert_eq!(problem(&body), "constant wider than its width");
    }
}
