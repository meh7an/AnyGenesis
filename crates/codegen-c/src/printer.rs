//! C text for the straight-line part of an IR body: values, expressions and the ops other
//! than control transfers.

use anygenesis_abi::Region;
use anygenesis_ir::{
    BinaryOp, Body, ConvertOp, Expr, Flag, FlagOp, Op, Reg, ShiftOp, UnaryOp, Value, Width,
};
use anygenesis_shared::Size;

pub const ADDRESS_MASK: u32 = 0x00FF_FFFF;

/// Whether `body` has an access the printed C may send to the runtime's bus: one whose
/// address is not known, or a constant one outside RAM (a write to ROM goes there too).
pub fn may_reach_bus(body: &Body, rom_size: u32) -> bool {
    let bus = |size: Size, address: Value, store: bool| match address {
        Value::Const { value, .. } => {
            match Region::of(value & ADDRESS_MASK, size.bytes(), rom_size) {
                Region::Ram => false,
                Region::Rom => store,
                Region::Bus => true,
            }
        }
        Value::Temp(_) => true,
    };
    body.ops.iter().any(|op| match *op {
        Op::Store { size, address, .. } => bus(size, address, true),
        Op::Let {
            expr: Expr::Load { size, address },
            ..
        } => bus(size, address, false),
        _ => false,
    })
}

/// Prints the values and ops of one body; its temporaries are named with `prefix`.
#[derive(Debug, Clone, Copy)]
pub struct Printer<'a> {
    pub body: &'a Body,
    pub prefix: &'static str,
    pub rom_size: u32,
    /// A C `int32_t` expression: the cycles already charged past the start of the
    /// instruction, which every device access subtracts so it sees that start.
    pub ahead: &'a str,
}

impl Printer<'_> {
    /// Returns a value as a C operand.
    pub fn value(&self, value: Value) -> String {
        match value {
            Value::Temp(temp) => format!("{}{}", self.prefix, temp.index()),
            Value::Const { value, .. } => constant(value),
        }
    }

    /// Returns the C statement for a non-control op, or `None` for a `Let` nothing reads
    /// that has no side effect. `Branch` and `Exit` are the caller's.
    pub fn op(&self, op: &Op, used: &[bool]) -> Option<String> {
        let statement = match *op {
            Op::Let { dst, expr } => {
                let text = self.expr(expr);
                if used.get(dst.index()).copied().unwrap_or(true) {
                    format!("uint32_t {}{} = {text};", self.prefix, dst.index())
                } else if matches!(expr, Expr::Load { .. }) {
                    format!("{text};")
                } else {
                    return None;
                }
            }
            Op::WriteReg { reg, size, value } => {
                let target = register(reg);
                let value = self.value(value);
                match (reg, size) {
                    (Reg::Data(_), Size::Byte | Size::Word) => {
                        let keep = !size.mask();
                        format!("{target} = ({target} & {}) | {value};", constant(keep))
                    }
                    _ => format!("{target} = {value};"),
                }
            }
            Op::SetFlag { flag, value } => {
                format!("s->{} = (uint8_t){};", flag_field(flag), self.value(value))
            }
            Op::WriteSr { value } => format!("ag_write_sr(s, {});", self.value(value)),
            Op::WriteCcr { value } => format!("ag_write_ccr(s, {});", self.value(value)),
            Op::Store {
                size,
                address,
                value,
            } => format!("{};", self.store(size, address, value)),
            Op::ResetDevices => "ag_reset_devices(s);".to_owned(),
            Op::Cycles { value } => format!("AG_CYCLES(s, (int32_t){});", self.value(value)),
            Op::Branch { .. } | Op::Exit(_) => return None,
        };
        Some(statement)
    }

    fn width(&self, value: Value) -> Width {
        self.body.width(value)
    }

    fn expr(&self, expr: Expr) -> String {
        match expr {
            Expr::ReadReg { reg, size } => masked(register(reg), size.into()),
            Expr::ReadFlag(flag) => format!("s->{}", flag_field(flag)),
            Expr::ReadSr => "ag_read_sr(s)".to_owned(),
            Expr::Supervisor => "ag_supervisor(s)".to_owned(),
            Expr::Load { size, address } => self.load(size, address),
            Expr::Unary { op, value } => self.unary(op, value),
            Expr::Binary { op, left, right } => self.binary(op, left, right),
            Expr::Shift { op, value, count } => {
                let width = self.width(value);
                let (name, needs_mask) = match op {
                    ShiftOp::Shl => ("ag_shl", true),
                    ShiftOp::Shr => ("ag_shr", false),
                    ShiftOp::Sar => ("ag_sar", true),
                };
                let call = format!(
                    "{name}({}, {}, {}u)",
                    self.value(value),
                    self.value(count),
                    width.bits()
                );
                if needs_mask {
                    masked(call, width)
                } else {
                    call
                }
            }
            Expr::Convert { op, width, value } => {
                let from = self.width(value);
                let operand = self.value(value);
                match op {
                    ConvertOp::ZeroExtend => operand,
                    ConvertOp::Truncate => masked(operand, width),
                    ConvertOp::SignExtend => {
                        masked(format!("ag_sext({operand}, {}u)", from.bits()), width)
                    }
                }
            }
            Expr::Select {
                condition,
                if_true,
                if_false,
            } => format!(
                "{} ? {} : {}",
                self.value(condition),
                self.value(if_true),
                self.value(if_false)
            ),
            Expr::Flag {
                op,
                left,
                right,
                result,
            } => {
                let name = match op {
                    FlagOp::AddCarry => "ag_add_carry",
                    FlagOp::AddOverflow => "ag_add_overflow",
                    FlagOp::SubCarry => "ag_sub_carry",
                    FlagOp::SubOverflow => "ag_sub_overflow",
                };
                format!(
                    "{name}({}, {}, {}, {}u)",
                    self.value(left),
                    self.value(right),
                    self.value(result),
                    self.width(left).bits()
                )
            }
        }
    }

    fn unary(&self, op: UnaryOp, value: Value) -> String {
        let width = self.width(value);
        let operand = self.value(value);
        match op {
            UnaryOp::Not if width == Width::Bool => format!("{operand} ^ 1u"),
            UnaryOp::Not => masked(format!("~{operand}"), width),
            UnaryOp::Neg => masked(format!("0u - {operand}"), width),
            UnaryOp::Ones => format!("(uint32_t)__builtin_popcount({operand})"),
        }
    }

    fn binary(&self, op: BinaryOp, left: Value, right: Value) -> String {
        let width = self.width(left);
        let (l, r) = (self.value(left), self.value(right));
        let bits = width.bits();
        match op {
            BinaryOp::Add => masked(format!("{l} + {r}"), width),
            BinaryOp::Sub => masked(format!("{l} - {r}"), width),
            BinaryOp::Mul => masked(format!("{l} * {r}"), width),
            BinaryOp::And => format!("{l} & {r}"),
            BinaryOp::Or => format!("{l} | {r}"),
            BinaryOp::Xor => format!("{l} ^ {r}"),
            BinaryOp::DivU => format!("ag_divu({l}, {r})"),
            BinaryOp::RemU => format!("ag_remu({l}, {r})"),
            BinaryOp::DivS => masked(format!("ag_divs({l}, {r}, {bits}u)"), width),
            BinaryOp::RemS => masked(format!("ag_rems({l}, {r}, {bits}u)"), width),
            BinaryOp::Eq => format!("(uint32_t)({l} == {r})"),
            BinaryOp::Ne => format!("(uint32_t)({l} != {r})"),
            BinaryOp::LtU => format!("(uint32_t)({l} < {r})"),
            BinaryOp::LtS => format!("ag_lts({l}, {r}, {bits}u)"),
            BinaryOp::DivuTime => format!("ag_divu_time({l}, {r})"),
            BinaryOp::DivsTime => format!("ag_divs_time({l}, {r})"),
        }
    }

    /// A read; one that can reach a device passes [`Printer::ahead`].
    fn load(&self, size: Size, address: Value) -> String {
        let bits = size.bytes() * 8;
        let ahead = self.ahead;
        match address {
            Value::Const { value, .. } => {
                let address = constant(value & ADDRESS_MASK);
                match Region::of(value & ADDRESS_MASK, size.bytes(), self.rom_size) {
                    Region::Ram => format!("ag_ram_read{bits}(s, {address})"),
                    Region::Rom => format!("ag_rom_read{bits}(s, {address})"),
                    Region::Bus => format!("ag_bus_read{bits}(s, {address}, {ahead})"),
                }
            }
            Value::Temp(_) => format!("ag_load{bits}(s, {}, {ahead})", self.value(address)),
        }
    }

    fn store(&self, size: Size, address: Value, value: Value) -> String {
        let bits = size.bytes() * 8;
        let value = self.value(value);
        let ahead = self.ahead;
        match address {
            Value::Const { value: raw, .. } => {
                let address = constant(raw & ADDRESS_MASK);
                match Region::of(raw & ADDRESS_MASK, size.bytes(), self.rom_size) {
                    Region::Ram => format!("ag_ram_write{bits}(s, {address}, {value})"),
                    Region::Rom | Region::Bus => {
                        format!("ag_bus_write{bits}(s, {address}, {value}, {ahead})")
                    }
                }
            }
            Value::Temp(_) => format!(
                "ag_store{bits}(s, {}, {value}, {ahead})",
                self.value(address)
            ),
        }
    }
}

/// Returns, for each temporary of `body`, whether a printed op reads it: any op but a `Let`,
/// or the `Let` of a temporary that is read in turn or of a load (printed for its access
/// even when its value is dead). A temporary is assigned once, before it is read, so one
/// pass from the end finds them all.
pub fn used_temps(body: &Body) -> Vec<bool> {
    let mut used = vec![false; body.temps.len()];
    for op in body.ops.iter().rev() {
        if let Op::Let { dst, expr } = *op
            && !matches!(expr, Expr::Load { .. })
            && !used.get(dst.index()).copied().unwrap_or(true)
        {
            continue;
        }
        let mut mark = |value: Value| {
            if let Value::Temp(temp) = value
                && let Some(slot) = used.get_mut(temp.index())
            {
                *slot = true;
            }
        };
        match *op {
            Op::Let { expr, .. } => match expr {
                Expr::ReadReg { .. } | Expr::ReadFlag(_) | Expr::ReadSr | Expr::Supervisor => {}
                Expr::Load { address, .. } => mark(address),
                Expr::Unary { value, .. } | Expr::Convert { value, .. } => mark(value),
                Expr::Binary { left, right, .. } => {
                    mark(left);
                    mark(right);
                }
                Expr::Shift { value, count, .. } => {
                    mark(value);
                    mark(count);
                }
                Expr::Select {
                    condition,
                    if_true,
                    if_false,
                } => {
                    mark(condition);
                    mark(if_true);
                    mark(if_false);
                }
                Expr::Flag {
                    left,
                    right,
                    result,
                    ..
                } => {
                    mark(left);
                    mark(right);
                    mark(result);
                }
            },
            Op::WriteReg { value, .. }
            | Op::SetFlag { value, .. }
            | Op::WriteSr { value }
            | Op::WriteCcr { value }
            | Op::Cycles { value } => mark(value),
            Op::Store { address, value, .. } => {
                mark(address);
                mark(value);
            }
            Op::ResetDevices => {}
            Op::Branch { condition, control } => {
                mark(condition);
                control_values(control, &mut mark);
            }
            Op::Exit(control) => control_values(control, &mut mark),
        }
    }
    used
}

fn control_values(control: anygenesis_ir::Control, mark: &mut impl FnMut(Value)) {
    use anygenesis_ir::Control;
    match control {
        Control::JumpIndirect(value)
        | Control::CallIndirect { target: value, .. }
        | Control::Return { target: value, .. } => mark(value),
        Control::Jump(_)
        | Control::Call { .. }
        | Control::Exception { .. }
        | Control::Stop { .. } => {}
    }
}

/// Returns a constant as a C unsigned literal.
pub fn constant(value: u32) -> String {
    format!("{value:#X}u").replacen("0X", "0x", 1)
}

/// Returns `text` limited to `width`'s bits.
pub fn masked(text: String, width: Width) -> String {
    if width == Width::Long {
        text
    } else {
        format!("({text}) & {}", constant(width.mask()))
    }
}

fn register(reg: Reg) -> String {
    match reg {
        Reg::Data(n) => format!("s->d[{}]", n & 7),
        Reg::Address(n) => format!("s->a[{}]", n & 7),
        Reg::InactiveSp => "s->inactive_sp".to_owned(),
    }
}

fn flag_field(flag: Flag) -> &'static str {
    match flag {
        Flag::X => "x",
        Flag::N => "n",
        Flag::Z => "z",
        Flag::V => "v",
        Flag::C => "c",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants_print_as_lowercase_prefixed_hex() {
        assert_eq!(constant(0xFF00), "0xFF00u");
        assert_eq!(constant(0), "0x0u");
    }
}
