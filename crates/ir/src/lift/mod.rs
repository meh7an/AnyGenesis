//! Lifting decoded instructions into the IR.
//!
//! Each instruction becomes one straight-line [`Body`]. Effects happen in the order the
//! 68000 performs them where that order is visible in the final state: privilege checks
//! come first, an address register's post-increment or pre-decrement is applied when its
//! effective address is computed (before a later trap or a read of the same register), and
//! flags are set before a trap that pushes them.

mod alu;
mod data;
mod flow;
mod operand;
mod shift;

use anygenesis_m68k::{Instruction, Mnemonic, Operand};
use anygenesis_shared::{Address, Size};

use crate::{
    builder::Builder,
    op::{Cycles, Flag, Lifted, Value},
};

/// Why an instruction did not lift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LiftError {
    /// The instruction's operands or size are not a combination the decoder produces. Only
    /// an `Instruction` modified after decoding can cause this.
    #[error("{mnemonic:?} with these operands or size is not a decoded 68000 instruction")]
    Malformed { mnemonic: Mnemonic },
}

/// Lifts `instruction`, located at `address`, into the IR.
///
/// # Errors
/// Returns [`LiftError::Malformed`] for operand or size combinations the decoder never
/// produces; every decoded instruction lifts.
pub fn lift(instruction: &Instruction, address: Address) -> Result<Lifted, LiftError> {
    let mut lifter = Lifter {
        instruction,
        address,
        out: Builder::default(),
    };
    lifter.lift()?;
    Ok(Lifted {
        address,
        length: instruction.length(),
        body: lifter.out.finish(),
        cycles: Cycles::default(),
    })
}

/// The exception vectors the IR raises.
pub(crate) mod vector {
    pub(crate) const ILLEGAL: u8 = 4;
    pub(crate) const ZERO_DIVIDE: u8 = 5;
    pub(crate) const CHK: u8 = 6;
    pub(crate) const TRAPV: u8 = 7;
    pub(crate) const PRIVILEGE: u8 = 8;
    pub(crate) const LINE_A: u8 = 10;
    pub(crate) const LINE_F: u8 = 11;
    pub(crate) const TRAP_BASE: u8 = 32;
}

struct Lifter<'a> {
    instruction: &'a Instruction,
    address: Address,
    out: Builder,
}

impl Lifter<'_> {
    fn lift(&mut self) -> Result<(), LiftError> {
        match self.instruction.mnemonic {
            Mnemonic::Add | Mnemonic::Addi | Mnemonic::Addq | Mnemonic::Adda => {
                self.add_sub(alu::Arithmetic::Add)
            }
            Mnemonic::Sub | Mnemonic::Subi | Mnemonic::Subq | Mnemonic::Suba => {
                self.add_sub(alu::Arithmetic::Sub)
            }
            Mnemonic::Addx => self.extended(alu::Arithmetic::Add),
            Mnemonic::Subx => self.extended(alu::Arithmetic::Sub),
            Mnemonic::Negx => self.negate(true),
            Mnemonic::Neg => self.negate(false),
            Mnemonic::Cmp | Mnemonic::Cmpi | Mnemonic::Cmpm | Mnemonic::Cmpa => self.compare(),
            Mnemonic::And | Mnemonic::Andi => self.logic(alu::Logic::And),
            Mnemonic::Or | Mnemonic::Ori => self.logic(alu::Logic::Or),
            Mnemonic::Eor | Mnemonic::Eori => self.logic(alu::Logic::Eor),
            Mnemonic::Not => self.not(),
            Mnemonic::Clr => self.clear(),
            Mnemonic::Tst => self.test(),
            Mnemonic::Tas => self.test_and_set(),
            Mnemonic::Ext => self.extend(),
            Mnemonic::Swap => self.swap(),
            Mnemonic::Mulu => self.multiply(false),
            Mnemonic::Muls => self.multiply(true),
            Mnemonic::Divu => self.divide(false),
            Mnemonic::Divs => self.divide(true),
            Mnemonic::Chk => self.check(),
            Mnemonic::Abcd => self.bcd(alu::Arithmetic::Add),
            Mnemonic::Sbcd => self.bcd(alu::Arithmetic::Sub),
            Mnemonic::Nbcd => self.negate_bcd(),
            Mnemonic::Asl => self.shift(shift::Kind::Asl),
            Mnemonic::Asr => self.shift(shift::Kind::Asr),
            Mnemonic::Lsl => self.shift(shift::Kind::Lsl),
            Mnemonic::Lsr => self.shift(shift::Kind::Lsr),
            Mnemonic::Rol => self.shift(shift::Kind::Rol),
            Mnemonic::Ror => self.shift(shift::Kind::Ror),
            Mnemonic::Roxl => self.shift(shift::Kind::Roxl),
            Mnemonic::Roxr => self.shift(shift::Kind::Roxr),
            Mnemonic::Btst => self.bit(shift::BitOp::Test),
            Mnemonic::Bchg => self.bit(shift::BitOp::Change),
            Mnemonic::Bclr => self.bit(shift::BitOp::Clear),
            Mnemonic::Bset => self.bit(shift::BitOp::Set),
            Mnemonic::Move => self.move_(),
            Mnemonic::Movea => self.move_address(),
            Mnemonic::Moveq => self.move_quick(),
            Mnemonic::Movem => self.move_multiple(),
            Mnemonic::Movep => self.move_peripheral(),
            Mnemonic::Lea => self.load_address(),
            Mnemonic::Pea => self.push_address(),
            Mnemonic::Exg => self.exchange(),
            Mnemonic::Link => self.link(),
            Mnemonic::Unlk => self.unlink(),
            Mnemonic::Bra => self.branch_always(),
            Mnemonic::Bsr => self.branch_subroutine(),
            Mnemonic::Bcc(condition) => self.branch_conditional(condition),
            Mnemonic::Dbcc(condition) => self.decrement_and_branch(condition),
            Mnemonic::Scc(condition) => self.set_conditional(condition),
            Mnemonic::Jmp => self.jump(),
            Mnemonic::Jsr => self.jump_subroutine(),
            Mnemonic::Rts => self.return_subroutine(),
            Mnemonic::Rtr => self.return_restore(),
            Mnemonic::Rte => self.return_exception(),
            Mnemonic::Trap => self.trap(),
            Mnemonic::Trapv => self.trap_overflow(),
            Mnemonic::Illegal => self.exception_here(vector::ILLEGAL),
            Mnemonic::LineA => self.exception_here(vector::LINE_A),
            Mnemonic::LineF => self.exception_here(vector::LINE_F),
            Mnemonic::Stop => self.stop(),
            Mnemonic::Reset => self.reset(),
            Mnemonic::Nop => Ok(()),
        }
    }

    fn malformed(&self) -> LiftError {
        LiftError::Malformed {
            mnemonic: self.instruction.mnemonic,
        }
    }

    fn size(&self) -> Result<Size, LiftError> {
        self.instruction.size.ok_or_else(|| self.malformed())
    }

    fn operand(&self, slot: usize) -> Result<Operand, LiftError> {
        self.instruction.operands[slot].ok_or_else(|| self.malformed())
    }

    /// The address of the next instruction.
    fn next(&self) -> Address {
        self.address.wrapping_add(self.instruction.length())
    }

    /// Sets N and Z from `result`, clears V and C: the flags of moves and logic operations.
    fn set_logic_flags(&mut self, result: Value) {
        self.set_nz(result);
        self.out.set_flag(Flag::V, Builder::boolean(false));
        self.out.set_flag(Flag::C, Builder::boolean(false));
    }

    fn set_nz(&mut self, result: Value) {
        let negative = self.out.is_negative(result);
        self.out.set_flag(Flag::N, negative);
        let zero = self.out.is_zero(result);
        self.out.set_flag(Flag::Z, zero);
    }
}
