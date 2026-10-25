use anygenesis_shared::{Address, Size};

/// The width of an IR value: a sized 68000 operand or a single condition bit.
///
/// Not `#[non_exhaustive]`, like `Size`: the interpreter, flag liveness and the C emitter must
/// match every width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Width {
    Bool,
    Byte,
    Word,
    Long,
}

impl Width {
    /// Returns the number of bits a value of this width holds.
    pub const fn bits(self) -> u32 {
        match self {
            Self::Bool => 1,
            Self::Byte => 8,
            Self::Word => 16,
            Self::Long => 32,
        }
    }

    /// Returns a mask covering the value's bits.
    pub const fn mask(self) -> u32 {
        u32::MAX >> (32 - self.bits())
    }

    /// Returns the operand size, or `None` for `Bool`.
    pub const fn size(self) -> Option<Size> {
        match self {
            Self::Bool => None,
            Self::Byte => Some(Size::Byte),
            Self::Word => Some(Size::Word),
            Self::Long => Some(Size::Long),
        }
    }
}

impl From<Size> for Width {
    fn from(size: Size) -> Self {
        match size {
            Size::Byte => Self::Byte,
            Size::Word => Self::Word,
            Size::Long => Self::Long,
        }
    }
}

/// A temporary, local to one [`Body`] and assigned exactly once, by the `Let` at its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Temp(pub(crate) u16);

impl Temp {
    /// Returns the temporary's index into [`Body::temps`].
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An operand of an IR operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Value {
    Temp(Temp),
    /// A constant whose bits fit `width` (`Bool` constants are 0 or 1).
    Const {
        width: Width,
        value: u32,
    },
}

/// A CPU register the IR reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reg {
    /// D0-D7.
    Data(u8),
    /// A0-A7; A7 is the active stack pointer (SSP in supervisor mode, USP in user mode).
    Address(u8),
    /// The stack pointer of the other mode: USP while in supervisor mode, SSP in user mode.
    /// A status register write that changes S swaps it with A7.
    InactiveSp,
}

/// A condition code flag. Each lives on its own, so flag liveness can drop dead writes; the
/// status register is assembled from them when read and split into them when written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    X,
    N,
    Z,
    V,
    C,
}

impl Flag {
    /// Every flag, in status register order from bit 4 down to bit 0.
    pub const ALL: [Self; 5] = [Self::X, Self::N, Self::Z, Self::V, Self::C];

    /// Returns the flag's bit position in the status register.
    pub const fn bit(self) -> u32 {
        match self {
            Self::X => 4,
            Self::N => 3,
            Self::Z => 2,
            Self::V => 1,
            Self::C => 0,
        }
    }
}

/// The status register bits that exist on the 68000: T, S, the interrupt mask and XNZVC.
/// Writes drop the others, and reads return them as zero.
pub const SR_MASK: u32 = 0xA71F;

/// The supervisor bit of the status register.
pub const SR_SUPERVISOR: u32 = 0x2000;

/// The trace bit of the status register. It is stored, but no trace exception is taken
/// (the CPU fidelity bar in PROGRESS.md).
pub const SR_TRACE: u32 = 0x8000;

/// A one-operand operation; the result has the operand's width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Bitwise complement (logical not for `Bool`).
    Not,
    /// Two's complement negation, wrapping.
    Neg,
}

/// A two-operand operation over operands of one width.
///
/// Every operation is total, so no consumer needs a special case: arithmetic wraps, a
/// division by zero yields 0 (quotient and remainder), and a signed division of the most
/// negative value by -1 yields that value with remainder 0. Division truncates toward zero
/// and the remainder takes the dividend's sign, as on the 68000 and in C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    /// The low bits of the product (signed and unsigned products agree on them).
    Mul,
    DivU,
    RemU,
    DivS,
    RemS,
    /// Equality; the result is `Bool`.
    Eq,
    /// Inequality; the result is `Bool`.
    Ne,
    /// Unsigned less than; the result is `Bool`.
    LtU,
    /// Signed less than; the result is `Bool`.
    LtS,
}

impl BinaryOp {
    /// Returns whether the result is a `Bool` rather than the operands' width.
    pub const fn is_comparison(self) -> bool {
        matches!(self, Self::Eq | Self::Ne | Self::LtU | Self::LtS)
    }
}

/// A shift by a `Byte` count. Counts of the width or more are defined: `Shl` and `Shr` give
/// 0, `Sar` fills with the sign bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShiftOp {
    Shl,
    Shr,
    Sar,
}

/// A width conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConvertOp {
    /// To a wider width, filling with zeros; a `Bool` becomes 0 or 1.
    ZeroExtend,
    /// To a wider sized width, filling with the sign bit.
    SignExtend,
    /// To a narrower sized width, keeping the low bits.
    Truncate,
}

/// A carry or overflow flag of an addition or subtraction, from its two operands and its
/// result, all of one sized width. Taking the result into account makes them right for
/// ADDX/SUBX/NEGX as well, whose result includes X.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlagOp {
    /// Carry out of `left + right`: the top bit of `(l & r) | (!res & (l | r))`.
    AddCarry,
    /// Signed overflow of `left + right`: the top bit of `(l ^ res) & (r ^ res)`.
    AddOverflow,
    /// Borrow of `left - right`: the top bit of `(!l & r) | (res & !(l ^ r))`.
    SubCarry,
    /// Signed overflow of `left - right`: the top bit of `(l ^ r) & (l ^ res)`.
    SubOverflow,
}

/// A computation whose result a [`Op::Let`] assigns to a temporary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Expr {
    /// The low `size` bits of a register.
    ReadReg {
        reg: Reg,
        size: Size,
    },
    /// One flag (`Bool`).
    ReadFlag(Flag),
    /// The whole status register (`Word`), flags included.
    ReadSr,
    /// The S bit of the status register (`Bool`).
    Supervisor,
    /// A bus read; `address` is `Long` and only its low 24 bits reach the bus.
    Load {
        size: Size,
        address: Value,
    },
    Unary {
        op: UnaryOp,
        value: Value,
    },
    Binary {
        op: BinaryOp,
        left: Value,
        right: Value,
    },
    Shift {
        op: ShiftOp,
        value: Value,
        count: Value,
    },
    Convert {
        op: ConvertOp,
        width: Width,
        value: Value,
    },
    /// `if_true` when `condition` (`Bool`) holds, else `if_false`.
    Select {
        condition: Value,
        if_true: Value,
        if_false: Value,
    },
    /// A carry or overflow flag (`Bool`).
    Flag {
        op: FlagOp,
        left: Value,
        right: Value,
        result: Value,
    },
}

/// One step of an instruction, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// Assigns `expr` to `dst`.
    Let { dst: Temp, expr: Expr },
    /// Writes the low `size` bits of a register, keeping the others. Address registers and
    /// the inactive stack pointer are only written whole (`Long`).
    WriteReg { reg: Reg, size: Size, value: Value },
    /// Sets one flag from a `Bool`.
    SetFlag { flag: Flag, value: Value },
    /// Writes the status register from a `Word`, keeping the [`SR_MASK`] bits. When S
    /// changes, A7 and the inactive stack pointer swap.
    WriteSr { value: Value },
    /// Writes the five flags from the low bits of a `Byte` (MOVE to CCR, RTR, xxxI to CCR).
    WriteCcr { value: Value },
    /// A bus write; `address` is `Long`, `value` has the width of `size`.
    Store {
        size: Size,
        address: Value,
        value: Value,
    },
    /// RESET: asserts the reset line of the external devices; the CPU itself is unaffected.
    ResetDevices,
    /// Leaves the instruction through `control` when `condition` (`Bool`) holds.
    Branch { condition: Value, control: Control },
    /// Leaves the instruction through `control`. Only ever the last op; an instruction
    /// without it falls through to the next one.
    Exit(Control),
}

/// How control leaves an instruction other than by falling through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    /// Continues at a known address (BRA, Bcc, DBcc, JMP to an absolute or PC-relative
    /// address).
    Jump(Address),
    /// Continues at a computed `Long` address (JMP through a register).
    JumpIndirect(Value),
    /// A subroutine call whose return address is already pushed (BSR, JSR).
    Call {
        target: Address,
        return_address: Address,
    },
    /// A call to a computed `Long` address.
    CallIndirect {
        target: Value,
        return_address: Address,
    },
    /// Continues at the `Long` address popped from the stack.
    Return { kind: ReturnKind, target: Value },
    /// Takes an exception through [`crate::exception_entry`]: `pc` is the address pushed in
    /// the frame.
    Exception { vector: u8, pc: Address },
    /// STOP: waits for an interrupt, then continues at `resume`.
    Stop { resume: Address },
}

/// The instruction a [`Control::Return`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReturnKind {
    Rts,
    Rtr,
    Rte,
}

/// A straight-line op list with its temporaries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Body {
    /// The width of each temporary, by index.
    pub temps: Vec<Width>,
    pub ops: Vec<Op>,
}

impl Body {
    /// Returns the width of `value`. A temporary index out of range yields `Long`; the
    /// validator reports such temporaries.
    pub fn width(&self, value: Value) -> Width {
        match value {
            Value::Temp(temp) => self.temps.get(temp.index()).copied().unwrap_or(Width::Long),
            Value::Const { width, .. } => width,
        }
    }

    /// Returns whether control reaches the next instruction when no branch is taken.
    pub fn falls_through(&self) -> bool {
        !matches!(self.ops.last(), Some(Op::Exit(_)))
    }
}

/// Cycle estimates for one instruction, filled from timing tables in M2.2 (zero until then).
/// Approximate by design: the CPU fidelity bar does not require exact timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cycles {
    /// When the instruction falls through.
    pub not_taken: u16,
    /// When a `Branch` or `Exit` leaves it.
    pub taken: u16,
}

/// One instruction lifted to the IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifted {
    /// The instruction's address.
    pub address: Address,
    /// The instruction's length in bytes.
    pub length: u32,
    pub body: Body,
    pub cycles: Cycles,
}

impl Lifted {
    /// Returns the address of the instruction after this one.
    pub const fn next(&self) -> Address {
        self.address.wrapping_add(self.length)
    }
}

/// Returns the width of `expr`'s result, given the width of each operand.
pub(crate) fn result_width(expr: &Expr, width: impl Fn(Value) -> Width) -> Width {
    match *expr {
        Expr::ReadReg { size, .. } | Expr::Load { size, .. } => size.into(),
        Expr::ReadFlag(_) | Expr::Supervisor | Expr::Flag { .. } => Width::Bool,
        Expr::ReadSr => Width::Word,
        Expr::Unary { value, .. } | Expr::Shift { value, .. } => width(value),
        Expr::Binary { op, left, .. } if !op.is_comparison() => width(left),
        Expr::Binary { .. } => Width::Bool,
        Expr::Convert { width, .. } => width,
        Expr::Select { if_true, .. } => width(if_true),
    }
}
