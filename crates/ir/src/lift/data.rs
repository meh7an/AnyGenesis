//! Moves, MOVEM, MOVEP, LEA, PEA, EXG, LINK and UNLK.

use anygenesis_m68k::{Ea, Operand, RegisterList};
use anygenesis_shared::Size;

use super::{LiftError, Lifter, operand::Place};
use crate::op::{BinaryOp, ConvertOp, Op, Reg, ShiftOp, Value, Width};

/// The register a MOVEM mask bit selects: bits 0-7 are D0-D7, bits 8-15 A0-A7.
fn movem_register(bit: u8) -> Reg {
    if bit < 8 {
        Reg::Data(bit)
    } else {
        Reg::Address(bit - 8)
    }
}

impl Lifter<'_> {
    /// MOVE between effective addresses, and the SR, CCR and USP forms.
    pub(super) fn move_(&mut self) -> Result<(), LiftError> {
        match self.instruction.operands {
            // MOVE from SR is unprivileged on the 68000.
            [Some(Operand::Sr), Some(_)] => {
                let sr = self.read(Place::Sr, Size::Word);
                let place = self.place(1, Size::Word)?;
                self.write(place, Size::Word, sr)
            }
            [Some(_), Some(Operand::Ccr)] => {
                let value = self.source(0, Size::Word)?;
                let ccr = self.out.convert(ConvertOp::Truncate, Width::Byte, value);
                self.out.op(Op::WriteCcr { value: ccr });
                Ok(())
            }
            [Some(_), Some(Operand::Sr)] => {
                self.require_supervisor();
                let value = self.source(0, Size::Word)?;
                self.out.op(Op::WriteSr { value });
                Ok(())
            }
            [Some(_), Some(Operand::Usp)] => {
                self.require_supervisor();
                let number = self.address_register(0)?;
                let value = self.out.read_reg(Reg::Address(number), Size::Long);
                self.out.write_reg(Reg::InactiveSp, Size::Long, value);
                Ok(())
            }
            [Some(Operand::Usp), Some(_)] => {
                self.require_supervisor();
                let number = self.address_register(1)?;
                let value = self.out.read_reg(Reg::InactiveSp, Size::Long);
                self.out.write_reg(Reg::Address(number), Size::Long, value);
                Ok(())
            }
            _ => {
                let size = self.size()?;
                let value = self.source(0, size)?;
                let place = self.place(1, size)?;
                self.write(place, size, value)?;
                self.set_logic_flags(value);
                Ok(())
            }
        }
    }

    /// MOVEA: the source is sign-extended to the whole register; no flags.
    pub(super) fn move_address(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let value = self.source(0, size)?;
        let number = self.address_register(1)?;
        let value = self.out.sign_extend_long(value);
        self.out.write_reg(Reg::Address(number), Size::Long, value);
        Ok(())
    }

    pub(super) fn move_quick(&mut self) -> Result<(), LiftError> {
        let value = self.source(0, Size::Long)?;
        let number = self.data_register(1)?;
        self.out.write_reg(Reg::Data(number), Size::Long, value);
        self.set_logic_flags(value);
        Ok(())
    }

    /// MOVEM in both directions. Words loaded into registers are sign-extended to the
    /// whole register, data registers included. The 68000's extra word read past the last
    /// register is not modelled.
    pub(super) fn move_multiple(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let step = size.bytes();
        match self.instruction.operands {
            [Some(Operand::RegisterList(list)), Some(Operand::Ea(ea))] => {
                if let Ea::PreDecrement(number) = ea {
                    // Stored from A7 down to D0 at falling addresses. An itself, if listed,
                    // is stored with its value before the instruction, as on the 68000.
                    let base = Reg::Address(number);
                    let mut address = self.out.read_reg(base, Size::Long);
                    for bit in (0..16).rev().filter(|bit| list.0 & (1 << bit) != 0) {
                        address = self.out.binary_const(BinaryOp::Sub, address, step);
                        let value = self.out.read_reg(movem_register(bit), size);
                        self.out.store(size, address, value);
                    }
                    self.out.write_reg(base, Size::Long, address);
                } else {
                    let start = self.address_of(ea, 1)?;
                    self.store_registers(list, size, start);
                }
            }
            [Some(Operand::Ea(ea)), Some(Operand::RegisterList(list))] => {
                if let Ea::PostIncrement(number) = ea {
                    // The write-back follows the loads, so it wins when An is listed.
                    let base = Reg::Address(number);
                    let start = self.out.read_reg(base, Size::Long);
                    let end = self.load_registers(list, size, start);
                    self.out.write_reg(base, Size::Long, end);
                } else {
                    let start = self.address_of(ea, 0)?;
                    self.load_registers(list, size, start);
                }
            }
            _ => return Err(self.malformed()),
        }
        Ok(())
    }

    /// Stores the listed registers from D0 up at rising addresses.
    fn store_registers(&mut self, list: RegisterList, size: Size, start: Value) {
        let mut address = start;
        for (index, bit) in (0..16).filter(|bit| list.0 & (1 << bit) != 0).enumerate() {
            if index > 0 {
                address = self.out.binary_const(BinaryOp::Add, address, size.bytes());
            }
            let value = self.out.read_reg(movem_register(bit), size);
            self.out.store(size, address, value);
        }
    }

    /// Loads the listed registers from D0 up and returns the address after the last one.
    fn load_registers(&mut self, list: RegisterList, size: Size, start: Value) -> Value {
        let mut address = start;
        for bit in (0..16).filter(|bit| list.0 & (1 << bit) != 0) {
            let value = self.out.load(size, address);
            let value = self.out.sign_extend_long(value);
            self.out.write_reg(movem_register(bit), Size::Long, value);
            address = self.out.binary_const(BinaryOp::Add, address, size.bytes());
        }
        address
    }

    /// MOVEP: the register's bytes, high first, to or from every other byte from
    /// `(d16,An)`.
    pub(super) fn move_peripheral(&mut self) -> Result<(), LiftError> {
        let size = self.size()?;
        let count = size.bytes();
        match self.instruction.operands {
            [
                Some(Operand::Ea(Ea::DataRegister(number))),
                Some(Operand::Ea(memory)),
            ] => {
                let base = self.address_of(memory, 1)?;
                let value = self.out.read_reg(Reg::Data(number), size);
                for index in 0..count {
                    let byte = self
                        .out
                        .shift_const(ShiftOp::Shr, value, 8 * (count - 1 - index));
                    let byte = self.out.convert(ConvertOp::Truncate, Width::Byte, byte);
                    let address = self.out.binary_const(BinaryOp::Add, base, 2 * index);
                    self.out.store(Size::Byte, address, byte);
                }
            }
            [
                Some(Operand::Ea(memory)),
                Some(Operand::Ea(Ea::DataRegister(number))),
            ] => {
                let base = self.address_of(memory, 0)?;
                let mut value = None;
                for index in 0..count {
                    let address = self.out.binary_const(BinaryOp::Add, base, 2 * index);
                    let byte = self.out.load(Size::Byte, address);
                    let byte = self.out.convert(ConvertOp::ZeroExtend, size.into(), byte);
                    let byte = self
                        .out
                        .shift_const(ShiftOp::Shl, byte, 8 * (count - 1 - index));
                    value = Some(match value {
                        Some(high) => self.out.or(high, byte),
                        None => byte,
                    });
                }
                let value = value.ok_or_else(|| self.malformed())?;
                self.out.write_reg(Reg::Data(number), size, value);
            }
            _ => return Err(self.malformed()),
        }
        Ok(())
    }

    pub(super) fn load_address(&mut self) -> Result<(), LiftError> {
        let address = self.address_of(self.ea(0)?, 0)?;
        let number = self.address_register(1)?;
        self.out
            .write_reg(Reg::Address(number), Size::Long, address);
        Ok(())
    }

    pub(super) fn push_address(&mut self) -> Result<(), LiftError> {
        let address = self.address_of(self.ea(0)?, 0)?;
        self.push(Size::Long, address);
        Ok(())
    }

    pub(super) fn exchange(&mut self) -> Result<(), LiftError> {
        let [first, second] = [self.register(0)?, self.register(1)?];
        let first_value = self.out.read_reg(first, Size::Long);
        let second_value = self.out.read_reg(second, Size::Long);
        self.out.write_reg(first, Size::Long, second_value);
        self.out.write_reg(second, Size::Long, first_value);
        Ok(())
    }

    fn register(&self, slot: usize) -> Result<Reg, LiftError> {
        match self.ea(slot)? {
            Ea::DataRegister(number) => Ok(Reg::Data(number)),
            Ea::AddressRegister(number) => Ok(Reg::Address(number)),
            _ => Err(self.malformed()),
        }
    }

    /// LINK An,#d: pushes An, points An at it and moves A7 by `d`. For A7 the pushed value
    /// is A7 before the push, as SingleStepTests shows.
    pub(super) fn link(&mut self) -> Result<(), LiftError> {
        let number = self.address_register(0)?;
        let displacement = self.immediate(1)?;
        let reg = Reg::Address(number);
        let value = self.out.read_reg(reg, Size::Long);
        let frame = self.push(Size::Long, value);
        self.out.write_reg(reg, Size::Long, frame);
        let displacement = Size::Word.sign_extend(displacement) as u32;
        let sp = self.out.binary_const(BinaryOp::Add, frame, displacement);
        self.out.write_reg(Reg::Address(7), Size::Long, sp);
        Ok(())
    }

    /// UNLK An: A7 = An + 4 and An = the long at An; for A7 the loaded value wins.
    pub(super) fn unlink(&mut self) -> Result<(), LiftError> {
        let number = self.address_register(0)?;
        let reg = Reg::Address(number);
        let frame = self.out.read_reg(reg, Size::Long);
        let value = self.out.load(Size::Long, frame);
        let sp = self.out.binary_const(BinaryOp::Add, frame, 4);
        self.out.write_reg(Reg::Address(7), Size::Long, sp);
        self.out.write_reg(reg, Size::Long, value);
        Ok(())
    }
}
