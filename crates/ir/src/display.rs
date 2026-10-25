//! A readable text form of the IR, one op per line, for tests and debugging.
//!
//! `t3:w = add t1, t2` assigns a temporary (width suffix `b`, `w`, `l` or `bool`); sized
//! constants print as `$ff.b`, boolean ones as `true`/`false`; registers as `d0.w`, `a7.l`,
//! and the inactive stack pointer as `isp.l`.

use std::fmt;

use anygenesis_shared::Size;

use crate::op::{
    BinaryOp, Body, Control, ConvertOp, Expr, Flag, FlagOp, Lifted, Op, Reg, ReturnKind, ShiftOp,
    UnaryOp, Value, Width,
};

fn size_suffix(size: Size) -> &'static str {
    match size {
        Size::Byte => "b",
        Size::Word => "w",
        Size::Long => "l",
    }
}

fn width_suffix(width: Width) -> &'static str {
    match width.size() {
        Some(size) => size_suffix(size),
        None => "bool",
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Temp(temp) => write!(f, "t{}", temp.index()),
            Self::Const {
                width: Width::Bool,
                value,
            } => f.write_str(if value != 0 { "true" } else { "false" }),
            Self::Const { width, value } => write!(f, "${value:x}.{}", width_suffix(width)),
        }
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(number) => write!(f, "d{number}"),
            Self::Address(number) => write!(f, "a{number}"),
            Self::InactiveSp => f.write_str("isp"),
        }
    }
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::X => "x",
            Self::N => "n",
            Self::Z => "z",
            Self::V => "v",
            Self::C => "c",
        })
    }
}

impl UnaryOp {
    const fn name(self) -> &'static str {
        match self {
            Self::Not => "not",
            Self::Neg => "neg",
        }
    }
}

impl BinaryOp {
    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Mul => "mul",
            Self::DivU => "divu",
            Self::RemU => "remu",
            Self::DivS => "divs",
            Self::RemS => "rems",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::LtU => "ltu",
            Self::LtS => "lts",
        }
    }
}

impl ShiftOp {
    const fn name(self) -> &'static str {
        match self {
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::Sar => "sar",
        }
    }
}

impl ConvertOp {
    const fn name(self) -> &'static str {
        match self {
            Self::ZeroExtend => "zext",
            Self::SignExtend => "sext",
            Self::Truncate => "trunc",
        }
    }
}

impl FlagOp {
    const fn name(self) -> &'static str {
        match self {
            Self::AddCarry => "add_carry",
            Self::AddOverflow => "add_overflow",
            Self::SubCarry => "sub_carry",
            Self::SubOverflow => "sub_overflow",
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ReadReg { reg, size } => write!(f, "{reg}.{}", size_suffix(size)),
            Self::ReadFlag(flag) => write!(f, "flag {flag}"),
            Self::ReadSr => f.write_str("sr"),
            Self::Supervisor => f.write_str("supervisor"),
            Self::Load { size, address } => write!(f, "load.{} {address}", size_suffix(size)),
            Self::Unary { op, value } => write!(f, "{} {value}", op.name()),
            Self::Binary { op, left, right } => write!(f, "{} {left}, {right}", op.name()),
            Self::Shift { op, value, count } => write!(f, "{} {value}, {count}", op.name()),
            Self::Convert { op, value, .. } => write!(f, "{} {value}", op.name()),
            Self::Select {
                condition,
                if_true,
                if_false,
            } => write!(f, "select {condition}, {if_true}, {if_false}"),
            Self::Flag {
                op,
                left,
                right,
                result,
            } => write!(f, "{} {left}, {right}, {result}", op.name()),
        }
    }
}

impl fmt::Display for Control {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Jump(target) => write!(f, "jump ${:06x}", target.get()),
            Self::JumpIndirect(target) => write!(f, "jump {target}"),
            Self::Call {
                target,
                return_address,
            } => write!(
                f,
                "call ${:06x} return ${:06x}",
                target.get(),
                return_address.get()
            ),
            Self::CallIndirect {
                target,
                return_address,
            } => write!(f, "call {target} return ${:06x}", return_address.get()),
            Self::Return { kind, target } => {
                let kind = match kind {
                    ReturnKind::Rts => "rts",
                    ReturnKind::Rtr => "rtr",
                    ReturnKind::Rte => "rte",
                };
                write!(f, "return {kind} {target}")
            }
            Self::Exception { vector, pc } => write!(f, "exception {vector} pc ${:06x}", pc.get()),
            Self::Stop { resume } => write!(f, "stop resume ${:06x}", resume.get()),
        }
    }
}

impl Body {
    fn write_op(&self, f: &mut fmt::Formatter<'_>, op: &Op) -> fmt::Result {
        match *op {
            Op::Let { dst, expr } => {
                let width = width_suffix(self.width(Value::Temp(dst)));
                write!(f, "t{}:{width} = {expr}", dst.index())
            }
            Op::WriteReg { reg, size, value } => {
                write!(f, "{reg}.{} = {value}", size_suffix(size))
            }
            Op::SetFlag { flag, value } => write!(f, "flag {flag} = {value}"),
            Op::WriteSr { value } => write!(f, "sr = {value}"),
            Op::WriteCcr { value } => write!(f, "ccr = {value}"),
            Op::Store {
                size,
                address,
                value,
            } => write!(f, "store.{} {address}, {value}", size_suffix(size)),
            Op::ResetDevices => f.write_str("reset devices"),
            Op::Branch { condition, control } => write!(f, "if {condition} {control}"),
            Op::Exit(control) => write!(f, "{control}"),
        }
    }
}

impl fmt::Display for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for op in &self.ops {
            self.write_op(f, op)?;
            f.write_str("\n")?;
        }
        Ok(())
    }
}

impl fmt::Display for Lifted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "${:06x} ({} bytes)\n{}",
            self.address.get(),
            self.length,
            self.body
        )
    }
}
