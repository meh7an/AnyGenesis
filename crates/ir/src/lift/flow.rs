//! Branches, jumps, calls, returns, exceptions, STOP and RESET.

use anygenesis_m68k::{Condition, Operand};
use anygenesis_shared::{Address, Size};

use super::{LiftError, Lifter, vector};
use crate::{
    builder::Builder,
    op::{BinaryOp, Control, ConvertOp, Flag, Op, Reg, ReturnKind, Value, Width},
};

impl Lifter<'_> {
    fn branch_target(&self) -> Result<Address, LiftError> {
        self.instruction
            .branch_target(self.address)
            .ok_or_else(|| self.malformed())
    }

    /// Evaluates a condition from the flags it reads, so liveness sees exactly those.
    fn condition(&mut self, condition: Condition) -> Value {
        match condition {
            Condition::True => Builder::boolean(true),
            Condition::False => Builder::boolean(false),
            Condition::Higher => {
                let carry_or_zero = self.carry_or_zero();
                self.out.not(carry_or_zero)
            }
            Condition::LowerOrSame => self.carry_or_zero(),
            Condition::CarryClear => self.flag_clear(Flag::C),
            Condition::CarrySet => self.out.read_flag(Flag::C),
            Condition::NotEqual => self.flag_clear(Flag::Z),
            Condition::Equal => self.out.read_flag(Flag::Z),
            Condition::OverflowClear => self.flag_clear(Flag::V),
            Condition::OverflowSet => self.out.read_flag(Flag::V),
            Condition::Plus => self.flag_clear(Flag::N),
            Condition::Minus => self.out.read_flag(Flag::N),
            Condition::GreaterOrEqual => {
                let less = self.less();
                self.out.not(less)
            }
            Condition::Less => self.less(),
            Condition::Greater => {
                let less_or_equal = self.less_or_equal();
                self.out.not(less_or_equal)
            }
            Condition::LessOrEqual => self.less_or_equal(),
        }
    }

    fn flag_clear(&mut self, flag: Flag) -> Value {
        let value = self.out.read_flag(flag);
        self.out.not(value)
    }

    fn carry_or_zero(&mut self) -> Value {
        let carry = self.out.read_flag(Flag::C);
        let zero = self.out.read_flag(Flag::Z);
        self.out.or(carry, zero)
    }

    /// N xor V.
    fn less(&mut self) -> Value {
        let negative = self.out.read_flag(Flag::N);
        let overflow = self.out.read_flag(Flag::V);
        self.out.xor(negative, overflow)
    }

    fn less_or_equal(&mut self) -> Value {
        let zero = self.out.read_flag(Flag::Z);
        let less = self.less();
        self.out.or(zero, less)
    }

    pub(super) fn branch_always(&mut self) -> Result<(), LiftError> {
        let target = self.branch_target()?;
        self.out.exit(Control::Jump(target));
        Ok(())
    }

    pub(super) fn branch_subroutine(&mut self) -> Result<(), LiftError> {
        let target = self.branch_target()?;
        self.call(Builder::constant(Width::Long, target.get()));
        Ok(())
    }

    pub(super) fn branch_conditional(&mut self, condition: Condition) -> Result<(), LiftError> {
        let target = self.branch_target()?;
        let taken = self.condition(condition);
        self.out.branch(taken, Control::Jump(target));
        Ok(())
    }

    /// DBcc: when the condition is false, decrements Dn.w and branches unless it became -1.
    pub(super) fn decrement_and_branch(&mut self, condition: Condition) -> Result<(), LiftError> {
        let target = self.branch_target()?;
        let reg = Reg::Data(self.data_register(0)?);
        let done = self.condition(condition);
        let counter = self.out.read_reg(reg, Size::Word);
        let decremented = self.out.binary_const(BinaryOp::Sub, counter, 1);
        let value = self.out.select(done, counter, decremented);
        self.out.write_reg(reg, Size::Word, value);
        let looping = self.out.binary_const(BinaryOp::Ne, decremented, 0xFFFF);
        let not_done = self.out.not(done);
        let taken = self.out.and(not_done, looping);
        self.out.branch(taken, Control::Jump(target));
        Ok(())
    }

    /// Scc: 0xFF when the condition holds, else 0. The 68000 reads a memory operand first;
    /// that extra read is not modelled.
    pub(super) fn set_conditional(&mut self, condition: Condition) -> Result<(), LiftError> {
        let holds = self.condition(condition);
        let place = self.place(0, Size::Byte)?;
        let set = Builder::constant(Width::Byte, 0xFF);
        let clear = Builder::constant(Width::Byte, 0);
        let value = self.out.select(holds, set, clear);
        self.write(place, Size::Byte, value)
    }

    /// JMP: a constant address (absolute or PC-relative) is a known jump.
    pub(super) fn jump(&mut self) -> Result<(), LiftError> {
        let target = self.address_of(self.ea(0)?, 0)?;
        let control = match target {
            Value::Const { value, .. } => Control::Jump(Address::new(value)),
            Value::Temp(_) => Control::JumpIndirect(target),
        };
        self.out.exit(control);
        Ok(())
    }

    /// JSR: the target is computed before the return address is pushed.
    pub(super) fn jump_subroutine(&mut self) -> Result<(), LiftError> {
        let target = self.address_of(self.ea(0)?, 0)?;
        self.call(target);
        Ok(())
    }

    /// Pushes the return address and calls `target`, a known call when it is a constant.
    fn call(&mut self, target: Value) {
        let return_address = self.next();
        let pushed = Builder::constant(Width::Long, return_address.get());
        self.push(Size::Long, pushed);
        let control = match target {
            Value::Const { value, .. } => Control::Call {
                target: Address::new(value),
                return_address,
            },
            Value::Temp(_) => Control::CallIndirect {
                target,
                return_address,
            },
        };
        self.out.exit(control);
    }

    pub(super) fn return_subroutine(&mut self) -> Result<(), LiftError> {
        let target = self.pop(Size::Long);
        self.out.exit(Control::Return {
            kind: ReturnKind::Rts,
            target,
        });
        Ok(())
    }

    /// RTR: pops the condition codes (a word, its low byte) and the return address.
    pub(super) fn return_restore(&mut self) -> Result<(), LiftError> {
        let status = self.pop(Size::Word);
        let target = self.pop(Size::Long);
        let ccr = self.out.convert(ConvertOp::Truncate, Width::Byte, status);
        self.out.op(Op::WriteCcr { value: ccr });
        self.out.exit(Control::Return {
            kind: ReturnKind::Rtr,
            target,
        });
        Ok(())
    }

    /// RTE: pops SR and PC from the supervisor stack; the SR write comes last, since
    /// returning to user mode swaps the stack pointers.
    pub(super) fn return_exception(&mut self) -> Result<(), LiftError> {
        self.require_supervisor();
        let status = self.pop(Size::Word);
        let target = self.pop(Size::Long);
        self.out.op(Op::WriteSr { value: status });
        self.out.exit(Control::Return {
            kind: ReturnKind::Rte,
            target,
        });
        Ok(())
    }

    pub(super) fn trap(&mut self) -> Result<(), LiftError> {
        let number = match self.operand(0)? {
            Operand::Vector(number) => number,
            _ => return Err(self.malformed()),
        };
        self.out.exit(Control::Exception {
            vector: vector::TRAP_BASE + number,
            pc: self.next(),
        });
        Ok(())
    }

    pub(super) fn trap_overflow(&mut self) -> Result<(), LiftError> {
        let overflow = self.out.read_flag(Flag::V);
        self.out.branch(
            overflow,
            Control::Exception {
                vector: vector::TRAPV,
                pc: self.next(),
            },
        );
        Ok(())
    }

    /// ILLEGAL, line A and line F: the frame holds the instruction's own address.
    pub(super) fn exception_here(&mut self, vector: u8) -> Result<(), LiftError> {
        self.out.exit(Control::Exception {
            vector,
            pc: self.address,
        });
        Ok(())
    }

    pub(super) fn stop(&mut self) -> Result<(), LiftError> {
        self.require_supervisor();
        let status = self.immediate(0)?;
        self.out.op(Op::WriteSr {
            value: Builder::constant(Width::Word, status),
        });
        self.out.exit(Control::Stop {
            resume: self.next(),
        });
        Ok(())
    }

    pub(super) fn reset(&mut self) -> Result<(), LiftError> {
        self.require_supervisor();
        self.out.op(Op::ResetDevices);
        Ok(())
    }
}
