use anygenesis_shared::{Address, Size};

use crate::{
    builder::Builder,
    op::{BinaryOp, Body, Control, Expr, Op, Reg, SR_SUPERVISOR, SR_TRACE, Temp, Value, Width},
};

/// The interrupt mask bits of the status register.
const SR_INTERRUPT_MASK: u32 = 0x0700;

/// The vector of the level 1 autovector; level `n` uses this plus `n - 1`.
const AUTOVECTOR_BASE: u8 = 25;

/// Returns the entry sequence of a group 1 or 2 exception (TRAP, TRAPV, CHK, divide by
/// zero, privilege violation, ILLEGAL, line A and line F), the one definition every
/// [`Control::Exception`] runs.
///
/// It enters supervisor mode with tracing off (the interrupt mask is kept), pushes the
/// six-byte frame (SR at the lower address, then `pc`) on the supervisor stack, and leaves
/// through the handler address read from the vector table.
pub fn exception_entry(vector: u8, pc: Address) -> Body {
    let mut out = Builder::default();
    entry(
        &mut out,
        vector,
        Builder::constant(Width::Long, pc.get()),
        None,
    );
    out.finish()
}

/// Returns the entry sequence of an autovectored interrupt at `level` (1-7) taken before
/// the instruction at `pc`: [`exception_entry`] with the interrupt mask raised to `level`.
pub fn interrupt_entry(level: u8, pc: Address) -> Body {
    let mut out = Builder::default();
    entry(
        &mut out,
        AUTOVECTOR_BASE + level - 1,
        Builder::constant(Width::Long, pc.get()),
        Some(level),
    );
    out.finish()
}

/// [`interrupt_entry`] for a `pc` known only when the interrupt is taken: the returned
/// temporary, which no op assigns, holds it. Native code binds it before the body runs; the
/// validator and the interpreter do not accept the body.
pub fn interrupt_entry_at(level: u8) -> (Body, Temp) {
    let mut out = Builder::default();
    let pc = out.input(Width::Long);
    entry(
        &mut out,
        AUTOVECTOR_BASE + level - 1,
        Value::Temp(pc),
        Some(level),
    );
    (out.finish(), pc)
}

fn entry(out: &mut Builder, vector: u8, pc: Value, mask: Option<u8>) {
    let sr = out.emit(Expr::ReadSr);
    let supervisor = out.binary_const(BinaryOp::Or, sr, SR_SUPERVISOR);
    let mut entered = out.binary_const(BinaryOp::And, supervisor, !SR_TRACE);
    if let Some(level) = mask {
        let cleared = out.binary_const(BinaryOp::And, entered, !SR_INTERRUPT_MASK);
        entered = out.binary_const(BinaryOp::Or, cleared, u32::from(level & 7) << 8);
    }
    // The S change swaps the stack pointers first, so the frame lands on the SSP.
    out.op(Op::WriteSr { value: entered });
    let sp = out.read_reg(Reg::Address(7), Size::Long);
    let frame = out.binary_const(BinaryOp::Sub, sp, 6);
    out.store(Size::Word, frame, sr);
    let pc_slot = out.binary_const(BinaryOp::Add, frame, 2);
    out.store(Size::Long, pc_slot, pc);
    out.write_reg(Reg::Address(7), Size::Long, frame);
    let table_entry = Builder::constant(Width::Long, u32::from(vector) * 4);
    let handler = out.load(Size::Long, table_entry);
    out.exit(Control::JumpIndirect(handler));
}
