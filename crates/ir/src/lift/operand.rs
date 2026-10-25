//! Effective addresses and operand access.

use anygenesis_m68k::{Ea, IndexRegister, IndexSize, Operand, Register};
use anygenesis_shared::Size;

use super::{LiftError, Lifter};
use crate::{
    builder::Builder,
    op::{BinaryOp, ConvertOp, Expr, Op, Reg, Value, Width},
};

/// Where an operand lives once its effective address is computed.
#[derive(Debug, Clone, Copy)]
pub(super) enum Place {
    Register(Reg),
    Memory(Value),
    Immediate(u32),
    Ccr,
    Sr,
}

/// A7 stays word aligned: byte accesses through `(A7)+` and `-(A7)` move it by 2.
pub(super) fn step(register: u8, size: Size) -> u32 {
    match (register, size) {
        (7, Size::Byte) => 2,
        _ => size.bytes(),
    }
}

impl Lifter<'_> {
    pub(super) fn ea(&self, slot: usize) -> Result<Ea, LiftError> {
        match self.operand(slot)? {
            Operand::Ea(ea) => Ok(ea),
            _ => Err(self.malformed()),
        }
    }

    pub(super) fn data_register(&self, slot: usize) -> Result<u8, LiftError> {
        match self.ea(slot)? {
            Ea::DataRegister(number) => Ok(number),
            _ => Err(self.malformed()),
        }
    }

    pub(super) fn address_register(&self, slot: usize) -> Result<u8, LiftError> {
        match self.ea(slot)? {
            Ea::AddressRegister(number) => Ok(number),
            _ => Err(self.malformed()),
        }
    }

    pub(super) fn immediate(&self, slot: usize) -> Result<u32, LiftError> {
        match self.ea(slot)? {
            Ea::Immediate(value) => Ok(value),
            _ => Err(self.malformed()),
        }
    }

    /// Computes the operand in `slot`, applying an address register update.
    pub(super) fn place(&mut self, slot: usize, size: Size) -> Result<Place, LiftError> {
        match self.operand(slot)? {
            Operand::Ea(ea) => self.place_ea(ea, size, slot),
            Operand::Ccr => Ok(Place::Ccr),
            Operand::Sr => Ok(Place::Sr),
            _ => Err(self.malformed()),
        }
    }

    fn place_ea(&mut self, ea: Ea, size: Size, slot: usize) -> Result<Place, LiftError> {
        Ok(match ea {
            Ea::DataRegister(number) => Place::Register(Reg::Data(number)),
            Ea::AddressRegister(number) => Place::Register(Reg::Address(number)),
            Ea::PostIncrement(number) => {
                let reg = Reg::Address(number);
                let address = self.out.read_reg(reg, Size::Long);
                let updated = self
                    .out
                    .binary_const(BinaryOp::Add, address, step(number, size));
                self.out.write_reg(reg, Size::Long, updated);
                Place::Memory(address)
            }
            Ea::PreDecrement(number) => {
                let reg = Reg::Address(number);
                let current = self.out.read_reg(reg, Size::Long);
                let address = self
                    .out
                    .binary_const(BinaryOp::Sub, current, step(number, size));
                self.out.write_reg(reg, Size::Long, address);
                Place::Memory(address)
            }
            Ea::Immediate(value) => Place::Immediate(value),
            _ => Place::Memory(self.address_of(ea, slot)?),
        })
    }

    /// Computes the address of a memory operand without side effects: the control modes,
    /// `(An)` included. The other modes are malformed here.
    pub(super) fn address_of(&mut self, ea: Ea, slot: usize) -> Result<Value, LiftError> {
        let extension = self.instruction.extension_address(self.address, slot).get();
        Ok(match ea {
            Ea::Indirect(number) => self.out.read_reg(Reg::Address(number), Size::Long),
            Ea::Displacement {
                register,
                displacement,
            } => {
                let base = self.out.read_reg(Reg::Address(register), Size::Long);
                self.out
                    .binary_const(BinaryOp::Add, base, i32::from(displacement) as u32)
            }
            Ea::Index {
                register,
                displacement,
                index,
            } => {
                let base = self.out.read_reg(Reg::Address(register), Size::Long);
                let based =
                    self.out
                        .binary_const(BinaryOp::Add, base, i32::from(displacement) as u32);
                let index = self.index(index);
                self.out.binary(BinaryOp::Add, based, index)
            }
            Ea::AbsoluteShort(raw) => Builder::constant(Width::Long, i32::from(raw as i16) as u32),
            Ea::AbsoluteLong(raw) => Builder::constant(Width::Long, raw),
            Ea::PcDisplacement(displacement) => Builder::constant(
                Width::Long,
                extension.wrapping_add(i32::from(displacement) as u32),
            ),
            Ea::PcIndex {
                displacement,
                index,
            } => {
                let base = Builder::constant(
                    Width::Long,
                    extension.wrapping_add(i32::from(displacement) as u32),
                );
                let index = self.index(index);
                self.out.binary(BinaryOp::Add, base, index)
            }
            Ea::DataRegister(_)
            | Ea::AddressRegister(_)
            | Ea::PostIncrement(_)
            | Ea::PreDecrement(_)
            | Ea::Immediate(_) => return Err(self.malformed()),
        })
    }

    fn index(&mut self, index: IndexRegister) -> Value {
        let reg = match index.register {
            Register::Data(number) => Reg::Data(number),
            Register::Address(number) => Reg::Address(number),
        };
        match index.size {
            IndexSize::Long => self.out.read_reg(reg, Size::Long),
            IndexSize::Word => {
                let word = self.out.read_reg(reg, Size::Word);
                self.out.sign_extend_long(word)
            }
        }
    }

    pub(super) fn read(&mut self, place: Place, size: Size) -> Value {
        match place {
            Place::Register(reg) => self.out.read_reg(reg, size),
            Place::Memory(address) => self.out.load(size, address),
            Place::Immediate(value) => Builder::constant(size.into(), value),
            Place::Ccr => {
                let sr = self.out.emit(Expr::ReadSr);
                self.out.convert(ConvertOp::Truncate, Width::Byte, sr)
            }
            Place::Sr => self.out.emit(Expr::ReadSr),
        }
    }

    pub(super) fn write(
        &mut self,
        place: Place,
        size: Size,
        value: Value,
    ) -> Result<(), LiftError> {
        match place {
            Place::Register(reg @ Reg::Data(_)) => self.out.write_reg(reg, size, value),
            Place::Register(reg) => {
                let long = self.out.sign_extend_long(value);
                self.out.write_reg(reg, Size::Long, long);
            }
            Place::Memory(address) => self.out.store(size, address, value),
            Place::Ccr => self.out.op(Op::WriteCcr { value }),
            Place::Sr => self.out.op(Op::WriteSr { value }),
            Place::Immediate(_) => return Err(self.malformed()),
        }
        Ok(())
    }

    /// Reads the source operand in `slot`: an effective address or quick data.
    pub(super) fn source(&mut self, slot: usize, size: Size) -> Result<Value, LiftError> {
        match self.operand(slot)? {
            Operand::Quick(quick) => Ok(Builder::constant(size.into(), i32::from(quick) as u32)),
            _ => {
                let place = self.place(slot, size)?;
                Ok(self.read(place, size))
            }
        }
    }

    /// Pushes `value` (of `size`) onto the active stack and returns the new stack pointer.
    pub(super) fn push(&mut self, size: Size, value: Value) -> Value {
        let sp = self.out.read_reg(Reg::Address(7), Size::Long);
        let address = self.out.binary_const(BinaryOp::Sub, sp, size.bytes());
        self.out.store(size, address, value);
        self.out.write_reg(Reg::Address(7), Size::Long, address);
        address
    }

    /// Pops a value of `size` from the active stack.
    pub(super) fn pop(&mut self, size: Size) -> Value {
        let sp = self.out.read_reg(Reg::Address(7), Size::Long);
        let value = self.out.load(size, sp);
        let updated = self.out.binary_const(BinaryOp::Add, sp, size.bytes());
        self.out.write_reg(Reg::Address(7), Size::Long, updated);
        value
    }
}
