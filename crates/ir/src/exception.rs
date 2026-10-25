use anygenesis_shared::{Address, Size};

use crate::{
    builder::Builder,
    op::{BinaryOp, Body, Control, Expr, Op, Reg, SR_SUPERVISOR, SR_TRACE, Width},
};

/// Returns the entry sequence of a group 1 or 2 exception (TRAP, TRAPV, CHK, divide by
/// zero, privilege violation, ILLEGAL, line A and line F), the one definition every
/// [`Control::Exception`] runs.
///
/// It enters supervisor mode with tracing off (the interrupt mask is kept), pushes the
/// six-byte frame (SR at the lower address, then `pc`) on the supervisor stack, and leaves
/// through the handler address read from the vector table.
pub fn exception_entry(vector: u8, pc: Address) -> Body {
    let mut out = Builder::default();
    let sr = out.emit(Expr::ReadSr);
    let supervisor = out.binary_const(BinaryOp::Or, sr, SR_SUPERVISOR);
    let entered = out.binary_const(BinaryOp::And, supervisor, !SR_TRACE);
    // The S change swaps the stack pointers first, so the frame lands on the SSP.
    out.op(Op::WriteSr { value: entered });
    let sp = out.read_reg(Reg::Address(7), Size::Long);
    let frame = out.binary_const(BinaryOp::Sub, sp, 6);
    out.store(Size::Word, frame, sr);
    let pc_slot = out.binary_const(BinaryOp::Add, frame, 2);
    out.store(
        Size::Long,
        pc_slot,
        Builder::constant(Width::Long, pc.get()),
    );
    out.write_reg(Reg::Address(7), Size::Long, frame);
    let table_entry = Builder::constant(Width::Long, u32::from(vector) * 4);
    let handler = out.load(Size::Long, table_entry);
    out.exit(Control::JumpIndirect(handler));
    out.finish()
}
