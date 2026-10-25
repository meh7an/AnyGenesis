//! The IR: the single definition of 68000 instruction behaviour (Architecture Decision 1).
//!
//! Every decoded instruction lifts to a straight-line [`Body`] of ops over typed
//! temporaries that are local to it and assigned once. The only control inside an
//! instruction is [`Expr::Select`] for values and [`Op::Branch`] for conditional exits;
//! [`Op::Exit`] ends it, and an instruction without one falls through. Flags are separate
//! bits set by one [`Op::SetFlag`] each, so flag liveness can drop dead ones. Every
//! operation is total, so the interpreter and the C emitter need no special cases.
//!
//! The consumers: the M2.2 interpreter (and the runtime fallback built from it), M2.3 flag
//! liveness and the M2.4 C emitter.

mod builder;
mod display;
pub mod eval;
mod exception;
mod lift;
mod op;
mod validate;

pub use exception::exception_entry;
pub use lift::{LiftError, lift};
pub use op::{
    BinaryOp, Body, Control, ConvertOp, Cycles, Expr, Flag, FlagOp, Lifted, Op, Reg, ReturnKind,
    SR_MASK, SR_SUPERVISOR, SR_TRACE, ShiftOp, Temp, UnaryOp, Value, Width,
};
pub use validate::{ValidationError, validate};
