//! One step: an instruction, an interrupt acknowledge, or a no-op while halted.
//!
//! Opcodes decode by their bit fields x (7-6), y (5-3), z (2-0), p (5-4) and q (3), the
//! layout the Zilog opcode tables follow. Bus accesses advance the step's T-state count by
//! their machine cycle lengths (opcode fetch 4, memory 3, I/O 4); `internal` adds the
//! T-states an instruction spends without the bus, at the point where it spends them.

use crate::{
    Bus, Z80,
    alu::{self, AluOp, CARRY, HALF, PARITY, SIGN, SUBTRACT, X, XY, Y, ZERO, parity, sz53, sz53p},
};

/// What H, L and (HL) stand for: a DD or FD prefix swaps in IX or IY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Index {
    Hl,
    Ix,
    Iy,
}

/// Interrupt modes by the y field of ED 46-7E; the undefined "IM 0/1" encodings act as 0.
const INTERRUPT_MODES: [u8; 8] = [0, 0, 1, 2, 0, 0, 1, 2];

/// The address an NMI calls.
const NMI_VECTOR: u16 = 0x0066;

/// The address an IM 1 interrupt calls.
const IM1_VECTOR: u16 = 0x0038;

pub(crate) struct Step<'a, B: Bus + ?Sized> {
    cpu: &'a mut Z80,
    bus: &'a mut B,
    /// T-states taken so far.
    t: u32,
    flags_written: bool,
    ei: bool,
    ld_a_ir: bool,
}

impl<'a, B: Bus + ?Sized> Step<'a, B> {
    pub(crate) fn new(cpu: &'a mut Z80, bus: &'a mut B) -> Self {
        Self {
            cpu,
            bus,
            t: 0,
            flags_written: false,
            ei: false,
            ld_a_ir: false,
        }
    }

    pub(crate) fn run(mut self) -> u32 {
        if self.cpu.nmi_pending {
            self.cpu.nmi_pending = false;
            self.nmi();
        } else if self.cpu.int_line && self.cpu.iff1 && !self.cpu.ei {
            self.interrupt();
        } else if self.cpu.halted {
            // A halted Z80 keeps fetching at the address after HALT, discarding the byte.
            self.m1(self.cpu.pc);
        } else {
            self.instruction();
        }
        self.cpu.q = if self.flags_written { self.cpu.f } else { 0 };
        self.cpu.ei = self.ei;
        self.cpu.p = self.ld_a_ir;
        self.t
    }

    fn nmi(&mut self) {
        self.begin_interrupt();
        self.cpu.iff1 = false;
        self.m1(self.cpu.pc);
        self.internal(1);
        self.call(NMI_VECTOR);
    }

    /// Acknowledges a maskable interrupt. In IM 0 the acknowledged byte is taken as an RST
    /// instruction, which is what the 0xFF of an undriven bus is; other opcodes placed on
    /// the bus are not supported.
    fn interrupt(&mut self) {
        self.begin_interrupt();
        self.cpu.iff1 = false;
        self.cpu.iff2 = false;
        // The acknowledge cycle is an opcode fetch stretched by two wait states, with the
        // device's byte read during the second of them.
        let data = self.bus.acknowledge(self.t + 3);
        self.t += 6;
        self.increment_r();
        self.internal(1);
        match self.cpu.im {
            0 => self.call(u16::from(data & 0x38)),
            1 => self.call(IM1_VECTOR),
            _ => {
                let pc = self.cpu.pc;
                self.push(pc);
                let vector = u16::from_be_bytes([self.cpu.i, data]);
                self.cpu.pc = self.read_word(vector);
                self.cpu.wz = self.cpu.pc;
            }
        }
    }

    /// What every interrupt does first: leave HALT, and clear the P/V that LD A,I or
    /// LD A,R just set, as the NMOS Z80 does when an interrupt follows them.
    fn begin_interrupt(&mut self) {
        self.cpu.halted = false;
        if self.cpu.p {
            self.cpu.f &= !PARITY;
        }
    }

    // Bus cycles and timing.

    fn internal(&mut self, t_states: u32) {
        self.t += t_states;
    }

    fn increment_r(&mut self) {
        let r = self.cpu.r;
        self.cpu.r = (r & 0x80) | (r.wrapping_add(1) & 0x7F);
    }

    /// An opcode fetch: four T-states, the last two refreshing memory.
    fn m1(&mut self, address: u16) -> u8 {
        let value = self.bus.read(address, self.t + 1);
        self.t += 4;
        self.increment_r();
        value
    }

    fn fetch_opcode(&mut self) -> u8 {
        let pc = self.cpu.pc;
        self.cpu.pc = pc.wrapping_add(1);
        self.m1(pc)
    }

    fn read(&mut self, address: u16) -> u8 {
        let value = self.bus.read(address, self.t + 1);
        self.t += 3;
        value
    }

    fn write(&mut self, address: u16, value: u8) {
        self.bus.write(address, value, self.t + 1);
        self.t += 3;
    }

    fn input(&mut self, port: u16) -> u8 {
        let value = self.bus.input(port, self.t + 2);
        self.t += 4;
        value
    }

    fn output(&mut self, port: u16, value: u8) {
        self.bus.output(port, value, self.t + 2);
        self.t += 4;
    }

    fn fetch_byte(&mut self) -> u8 {
        let pc = self.cpu.pc;
        self.cpu.pc = pc.wrapping_add(1);
        self.read(pc)
    }

    fn fetch_word(&mut self) -> u16 {
        let low = self.fetch_byte();
        let high = self.fetch_byte();
        u16::from_le_bytes([low, high])
    }

    fn read_word(&mut self, address: u16) -> u16 {
        let low = self.read(address);
        let high = self.read(address.wrapping_add(1));
        u16::from_le_bytes([low, high])
    }

    fn write_word(&mut self, address: u16, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.write(address, low);
        self.write(address.wrapping_add(1), high);
    }

    fn push(&mut self, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.cpu.sp = self.cpu.sp.wrapping_sub(1);
        self.write(self.cpu.sp, high);
        self.cpu.sp = self.cpu.sp.wrapping_sub(1);
        self.write(self.cpu.sp, low);
    }

    fn pop(&mut self) -> u16 {
        let value = self.read_word(self.cpu.sp);
        self.cpu.sp = self.cpu.sp.wrapping_add(2);
        value
    }

    /// Pushes PC and jumps to `target`, as CALL, RST and interrupts do.
    fn call(&mut self, target: u16) {
        let pc = self.cpu.pc;
        self.push(pc);
        self.cpu.pc = target;
        self.cpu.wz = target;
    }

    fn set_flags(&mut self, flags: u8) {
        self.cpu.f = flags;
        self.flags_written = true;
    }

    // Registers.

    fn index_value(&self, index: Index) -> u16 {
        match index {
            Index::Hl => self.cpu.hl(),
            Index::Ix => self.cpu.ix,
            Index::Iy => self.cpu.iy,
        }
    }

    fn set_index_value(&mut self, index: Index, value: u16) {
        match index {
            Index::Hl => self.cpu.set_hl(value),
            Index::Ix => self.cpu.ix = value,
            Index::Iy => self.cpu.iy = value,
        }
    }

    /// Register `number` of B, C, D, E, H, L, -, A; H and L become the halves of IX or IY
    /// under a prefix.
    fn reg8(&self, number: u8, index: Index) -> u8 {
        let [high, low] = self.index_value(index).to_be_bytes();
        match number {
            0 => self.cpu.b,
            1 => self.cpu.c,
            2 => self.cpu.d,
            3 => self.cpu.e,
            4 => high,
            5 => low,
            7 => self.cpu.a,
            // Every caller decodes 6, the (HL) operand, as a memory access first.
            _ => unreachable!("register 6 is the memory operand"),
        }
    }

    fn set_reg8(&mut self, number: u8, index: Index, value: u8) {
        let [high, low] = self.index_value(index).to_be_bytes();
        match number {
            0 => self.cpu.b = value,
            1 => self.cpu.c = value,
            2 => self.cpu.d = value,
            3 => self.cpu.e = value,
            4 => self.set_index_value(index, u16::from_be_bytes([value, low])),
            5 => self.set_index_value(index, u16::from_be_bytes([high, value])),
            7 => self.cpu.a = value,
            // Every caller decodes 6, the (HL) operand, as a memory access first.
            _ => unreachable!("register 6 is the memory operand"),
        }
    }

    /// Register pair `p` of BC, DE, HL, SP.
    fn rp(&self, p: u8, index: Index) -> u16 {
        match p {
            0 => self.cpu.bc(),
            1 => self.cpu.de(),
            2 => self.index_value(index),
            _ => self.cpu.sp,
        }
    }

    fn set_rp(&mut self, p: u8, index: Index, value: u16) {
        match p {
            0 => self.cpu.set_bc(value),
            1 => self.cpu.set_de(value),
            2 => self.set_index_value(index, value),
            _ => self.cpu.sp = value,
        }
    }

    /// Register pair `p` of BC, DE, HL, AF, as PUSH and POP name them.
    fn rp2(&self, p: u8, index: Index) -> u16 {
        match p {
            3 => self.cpu.af(),
            p => self.rp(p, index),
        }
    }

    fn set_rp2(&mut self, p: u8, index: Index, value: u16) {
        match p {
            3 => self.cpu.set_af(value),
            p => self.set_rp(p, index, value),
        }
    }

    /// Condition `cc` of NZ, Z, NC, C, PO, PE, P, M.
    fn condition(&self, cc: u8) -> bool {
        let flag = [ZERO, ZERO, CARRY, CARRY, PARITY, PARITY, SIGN, SIGN][usize::from(cc)];
        (self.cpu.f & flag != 0) == (cc & 1 == 1)
    }

    /// Reads the displacement of (IX+d) or (IY+d) and returns the address, which WZ keeps.
    fn displaced(&mut self, index: Index) -> u16 {
        let displacement = self.fetch_byte() as i8;
        let address = self
            .index_value(index)
            .wrapping_add_signed(i16::from(displacement));
        self.cpu.wz = address;
        address
    }

    /// The address of the (HL) operand: HL itself, or (IX+d) after the five T-states the
    /// addition takes.
    fn memory_operand(&mut self, index: Index) -> u16 {
        match index {
            Index::Hl => self.cpu.hl(),
            _ => {
                let address = self.displaced(index);
                self.internal(5);
                address
            }
        }
    }

    // Decoding.

    fn instruction(&mut self) {
        let mut index = Index::Hl;
        let mut opcode = self.fetch_opcode();
        loop {
            index = match opcode {
                0xDD => Index::Ix,
                0xFD => Index::Iy,
                _ => break,
            };
            // SCF and CCF after a prefix see no flags written by the instruction before.
            self.cpu.q = 0;
            opcode = self.fetch_opcode();
        }
        match (opcode, index) {
            (0xCB, Index::Hl) => self.cb(),
            (0xCB, _) => self.indexed_cb(index),
            (0xED, _) => self.ed(),
            _ => self.main(opcode, index),
        }
    }

    fn main(&mut self, opcode: u8, index: Index) {
        let (x, y, z) = (opcode >> 6, (opcode >> 3) & 7, opcode & 7);
        match x {
            0 => self.main_x0(y, z, index),
            1 => self.load(y, z, index),
            2 => {
                let value = self.operand(z, index);
                self.accumulate(y, value);
            }
            _ => self.main_x3(y, z, index),
        }
    }

    fn main_x0(&mut self, y: u8, z: u8, index: Index) {
        let (p, q) = (y >> 1, y & 1);
        match z {
            0 => match y {
                0 => {}
                1 => {
                    let af = self.cpu.af();
                    self.cpu.set_af(self.cpu.af_);
                    self.cpu.af_ = af;
                }
                2 => {
                    self.internal(1);
                    self.cpu.b = self.cpu.b.wrapping_sub(1);
                    let taken = self.cpu.b != 0;
                    self.relative_jump(taken);
                }
                3 => self.relative_jump(true),
                _ => {
                    let taken = self.condition(y - 4);
                    self.relative_jump(taken);
                }
            },
            1 if q == 0 => {
                let value = self.fetch_word();
                self.set_rp(p, index, value);
            }
            1 => {
                let left = self.index_value(index);
                self.internal(7);
                let (result, flags) = alu::add16(left, self.rp(p, index), self.cpu.f);
                self.cpu.wz = left.wrapping_add(1);
                self.set_index_value(index, result);
                self.set_flags(flags);
            }
            2 => self.indirect_load(p, q, index),
            3 => {
                self.internal(2);
                let value = self.rp(p, index);
                let value = if q == 0 {
                    value.wrapping_add(1)
                } else {
                    value.wrapping_sub(1)
                };
                self.set_rp(p, index, value);
            }
            4 => self.modify(y, index, alu::inc8),
            5 => self.modify(y, index, alu::dec8),
            6 if y == 6 => {
                let address = match index {
                    Index::Hl => self.cpu.hl(),
                    _ => self.displaced(index),
                };
                let value = self.fetch_byte();
                if index != Index::Hl {
                    self.internal(2);
                }
                self.write(address, value);
            }
            6 => {
                let value = self.fetch_byte();
                self.set_reg8(y, index, value);
            }
            _ => self.accumulator_op(y),
        }
    }

    /// DJNZ, JR and JR cc after their condition: reads the displacement and jumps.
    fn relative_jump(&mut self, taken: bool) {
        let displacement = self.fetch_byte() as i8;
        if taken {
            self.internal(5);
            self.cpu.pc = self.cpu.pc.wrapping_add_signed(i16::from(displacement));
            self.cpu.wz = self.cpu.pc;
        }
    }

    /// LD (BC),A, LD (DE),A, LD (nn),HL, LD (nn),A and their loads the other way.
    fn indirect_load(&mut self, p: u8, q: u8, index: Index) {
        match (q, p) {
            (0, 0 | 1) => {
                let address = self.rp(p, index);
                let a = self.cpu.a;
                self.write(address, a);
                self.cpu.wz = u16::from_be_bytes([a, address.wrapping_add(1) as u8]);
            }
            (0, 2) => {
                let address = self.fetch_word();
                self.write_word(address, self.index_value(index));
                self.cpu.wz = address.wrapping_add(1);
            }
            (0, _) => {
                let address = self.fetch_word();
                let a = self.cpu.a;
                self.write(address, a);
                self.cpu.wz = u16::from_be_bytes([a, address.wrapping_add(1) as u8]);
            }
            (_, 0 | 1) => {
                let address = self.rp(p, index);
                self.cpu.a = self.read(address);
                self.cpu.wz = address.wrapping_add(1);
            }
            (_, 2) => {
                let address = self.fetch_word();
                let value = self.read_word(address);
                self.set_index_value(index, value);
                self.cpu.wz = address.wrapping_add(1);
            }
            _ => {
                let address = self.fetch_word();
                self.cpu.a = self.read(address);
                self.cpu.wz = address.wrapping_add(1);
            }
        }
    }

    /// INC or DEC of a register or of (HL), which takes one more T-state between the read
    /// and the write.
    fn modify(&mut self, y: u8, index: Index, operation: fn(u8, u8) -> (u8, u8)) {
        if y == 6 {
            let address = self.memory_operand(index);
            let value = self.read(address);
            self.internal(1);
            let (result, flags) = operation(value, self.cpu.f);
            self.write(address, result);
            self.set_flags(flags);
        } else {
            let (result, flags) = operation(self.reg8(y, index), self.cpu.f);
            self.set_reg8(y, index, result);
            self.set_flags(flags);
        }
    }

    /// RLCA, RRCA, RLA, RRA, DAA, CPL, SCF, CCF.
    fn accumulator_op(&mut self, y: u8) {
        let (a, flags) = (self.cpu.a, self.cpu.f);
        // SCF and CCF take X and Y from A, or from A and F when the instruction before
        // wrote no flags.
        let xy = ((self.cpu.q ^ flags) | a) & XY;
        let kept = flags & (SIGN | ZERO | PARITY);
        match y {
            0..=3 => {
                let (result, flags) = alu::rotate_a(y, a, flags);
                self.cpu.a = result;
                self.set_flags(flags);
            }
            4 => {
                let (result, flags) = alu::daa(a, flags);
                self.cpu.a = result;
                self.set_flags(flags);
            }
            5 => {
                let result = !a;
                self.cpu.a = result;
                self.set_flags(kept | (flags & CARRY) | HALF | SUBTRACT | (result & XY));
            }
            6 => self.set_flags(kept | xy | CARRY),
            _ => {
                let carry = flags & CARRY;
                self.set_flags(kept | xy | (carry << 4) | (carry ^ CARRY));
            }
        }
    }

    /// LD r,r', LD r,(HL), LD (HL),r and HALT. Next to (IX+d), H and L stay H and L.
    fn load(&mut self, y: u8, z: u8, index: Index) {
        match (y, z) {
            (6, 6) => self.cpu.halted = true,
            (_, 6) => {
                let address = self.memory_operand(index);
                let value = self.read(address);
                self.set_reg8(y, Index::Hl, value);
            }
            (6, _) => {
                let address = self.memory_operand(index);
                let value = self.reg8(z, Index::Hl);
                self.write(address, value);
            }
            _ => {
                let value = self.reg8(z, index);
                self.set_reg8(y, index, value);
            }
        }
    }

    /// The source operand `z` of an accumulator operation: a register or (HL).
    fn operand(&mut self, z: u8, index: Index) -> u8 {
        if z == 6 {
            let address = self.memory_operand(index);
            self.read(address)
        } else {
            self.reg8(z, index)
        }
    }

    fn accumulate(&mut self, y: u8, value: u8) {
        let (result, flags) = alu::alu(AluOp::from_bits(y), self.cpu.a, value, self.cpu.f);
        self.cpu.a = result;
        self.set_flags(flags);
    }

    fn main_x3(&mut self, y: u8, z: u8, index: Index) {
        let (p, q) = (y >> 1, y & 1);
        match z {
            0 => {
                self.internal(1);
                if self.condition(y) {
                    self.cpu.pc = self.pop();
                    self.cpu.wz = self.cpu.pc;
                }
            }
            1 if q == 0 => {
                let value = self.pop();
                self.set_rp2(p, index, value);
            }
            1 => match p {
                0 => {
                    self.cpu.pc = self.pop();
                    self.cpu.wz = self.cpu.pc;
                }
                1 => {
                    let cpu = &mut *self.cpu;
                    let (bc, de, hl) = (cpu.bc(), cpu.de(), cpu.hl());
                    cpu.set_bc(cpu.bc_);
                    cpu.set_de(cpu.de_);
                    cpu.set_hl(cpu.hl_);
                    (cpu.bc_, cpu.de_, cpu.hl_) = (bc, de, hl);
                }
                2 => self.cpu.pc = self.index_value(index),
                _ => {
                    self.internal(2);
                    self.cpu.sp = self.index_value(index);
                }
            },
            2 => {
                let target = self.fetch_word();
                self.cpu.wz = target;
                if self.condition(y) {
                    self.cpu.pc = target;
                }
            }
            3 => self.main_x3_z3(y, index),
            4 => {
                let target = self.fetch_word();
                self.cpu.wz = target;
                if self.condition(y) {
                    self.internal(1);
                    self.call(target);
                }
            }
            5 if q == 0 => {
                self.internal(1);
                let value = self.rp2(p, index);
                self.push(value);
            }
            5 => {
                // p = 0 is CALL nn; DD, ED and FD (p = 1-3) are dispatched as prefixes.
                let target = self.fetch_word();
                self.internal(1);
                self.call(target);
            }
            6 => {
                let value = self.fetch_byte();
                self.accumulate(y, value);
            }
            _ => {
                self.internal(1);
                self.call(u16::from(y) * 8);
            }
        }
    }

    /// JP nn, OUT (n),A, IN A,(n), EX (SP),HL, EX DE,HL, DI and EI (CB is a prefix).
    fn main_x3_z3(&mut self, y: u8, index: Index) {
        match y {
            0 => {
                let target = self.fetch_word();
                self.cpu.pc = target;
                self.cpu.wz = target;
            }
            2 => {
                let n = self.fetch_byte();
                let a = self.cpu.a;
                self.output(u16::from_be_bytes([a, n]), a);
                self.cpu.wz = u16::from_be_bytes([a, n.wrapping_add(1)]);
            }
            3 => {
                let n = self.fetch_byte();
                let port = u16::from_be_bytes([self.cpu.a, n]);
                self.cpu.a = self.input(port);
                self.cpu.wz = port.wrapping_add(1);
            }
            4 => {
                let sp = self.cpu.sp;
                let low = self.read(sp);
                let high = self.read(sp.wrapping_add(1));
                self.internal(1);
                let [old_low, old_high] = self.index_value(index).to_le_bytes();
                self.write(sp.wrapping_add(1), old_high);
                self.write(sp, old_low);
                self.internal(2);
                let value = u16::from_le_bytes([low, high]);
                self.set_index_value(index, value);
                self.cpu.wz = value;
            }
            5 => {
                let de = self.cpu.de();
                self.cpu.set_de(self.cpu.hl());
                self.cpu.set_hl(de);
            }
            6 => {
                self.cpu.iff1 = false;
                self.cpu.iff2 = false;
            }
            _ => {
                self.cpu.iff1 = true;
                self.cpu.iff2 = true;
                self.ei = true;
            }
        }
    }

    // CB prefix.

    fn cb(&mut self) {
        let opcode = self.fetch_opcode();
        let (x, y, z) = (opcode >> 6, (opcode >> 3) & 7, opcode & 7);
        if z == 6 {
            let address = self.cpu.hl();
            let value = self.read(address);
            self.internal(1);
            let xy_source = (self.cpu.wz >> 8) as u8;
            if let Some(result) = self.bit_operation(x, y, value, xy_source) {
                self.write(address, result);
            }
        } else {
            let value = self.reg8(z, Index::Hl);
            if let Some(result) = self.bit_operation(x, y, value, value) {
                self.set_reg8(z, Index::Hl, result);
            }
        }
    }

    /// DD CB d op and FD CB d op. Besides (IX+d), every operation but BIT also writes its
    /// result to register z unless z is 6.
    fn indexed_cb(&mut self, index: Index) {
        let address = self.displaced(index);
        let opcode = self.fetch_byte();
        self.internal(2);
        let (x, y, z) = (opcode >> 6, (opcode >> 3) & 7, opcode & 7);
        let value = self.read(address);
        self.internal(1);
        if let Some(result) = self.bit_operation(x, y, value, (address >> 8) as u8) {
            self.write(address, result);
            if z != 6 {
                self.set_reg8(z, Index::Hl, result);
            }
        }
    }

    /// A rotate or shift (x = 0), BIT (1), RES (2) or SET (3) of bit or kind `y`; returns
    /// the value to store back, which BIT has none of.
    fn bit_operation(&mut self, x: u8, y: u8, value: u8, xy_source: u8) -> Option<u8> {
        match x {
            0 => {
                let (result, flags) = alu::rotate(y, value, self.cpu.f);
                self.set_flags(flags);
                Some(result)
            }
            1 => {
                let flags = alu::bit(y, value, xy_source, self.cpu.f);
                self.set_flags(flags);
                None
            }
            2 => Some(value & !(1 << y)),
            _ => Some(value | (1 << y)),
        }
    }

    // ED prefix.

    fn ed(&mut self) {
        let opcode = self.fetch_opcode();
        let (x, y, z) = (opcode >> 6, (opcode >> 3) & 7, opcode & 7);
        match x {
            1 => self.ed_x1(y, z),
            2 if z <= 3 && y >= 4 => self.block(y, z),
            // Every other ED opcode is a no-op of eight T-states.
            _ => {}
        }
    }

    fn ed_x1(&mut self, y: u8, z: u8) {
        let (p, q) = (y >> 1, y & 1);
        match z {
            0 => {
                let bc = self.cpu.bc();
                let value = self.input(bc);
                self.cpu.wz = bc.wrapping_add(1);
                self.set_flags((self.cpu.f & CARRY) | sz53p(value));
                // IN (C) (y = 6) only sets the flags.
                if y != 6 {
                    self.set_reg8(y, Index::Hl, value);
                }
            }
            1 => {
                let bc = self.cpu.bc();
                // OUT (C),0 (y = 6) puts 0 on the bus on the NMOS Z80.
                let value = if y == 6 { 0 } else { self.reg8(y, Index::Hl) };
                self.output(bc, value);
                self.cpu.wz = bc.wrapping_add(1);
            }
            2 => {
                let left = self.cpu.hl();
                self.internal(7);
                let right = self.rp(p, Index::Hl);
                let (result, flags) = if q == 0 {
                    alu::sbc16(left, right, self.cpu.f)
                } else {
                    alu::adc16(left, right, self.cpu.f)
                };
                self.cpu.wz = left.wrapping_add(1);
                self.cpu.set_hl(result);
                self.set_flags(flags);
            }
            3 => {
                let address = self.fetch_word();
                if q == 0 {
                    self.write_word(address, self.rp(p, Index::Hl));
                } else {
                    let value = self.read_word(address);
                    self.set_rp(p, Index::Hl, value);
                }
                self.cpu.wz = address.wrapping_add(1);
            }
            4 => {
                let (result, flags) = alu::alu(AluOp::Sub, 0, self.cpu.a, 0);
                self.cpu.a = result;
                self.set_flags(flags);
            }
            5 => {
                // RETN and RETI both restore IFF1 from IFF2.
                self.cpu.pc = self.pop();
                self.cpu.wz = self.cpu.pc;
                self.cpu.iff1 = self.cpu.iff2;
            }
            6 => self.cpu.im = INTERRUPT_MODES[usize::from(y)],
            _ => self.ed_x1_z7(y),
        }
    }

    /// LD I,A, LD R,A, LD A,I, LD A,R, RRD, RLD and two no-ops.
    fn ed_x1_z7(&mut self, y: u8) {
        match y {
            0 => {
                self.internal(1);
                self.cpu.i = self.cpu.a;
            }
            1 => {
                self.internal(1);
                self.cpu.r = self.cpu.a;
            }
            2 | 3 => {
                self.internal(1);
                let value = if y == 2 { self.cpu.i } else { self.cpu.r };
                self.cpu.a = value;
                let iff2 = if self.cpu.iff2 { PARITY } else { 0 };
                self.set_flags((self.cpu.f & CARRY) | sz53(value) | iff2);
                self.ld_a_ir = true;
            }
            4 | 5 => {
                let address = self.cpu.hl();
                let value = self.read(address);
                self.internal(4);
                let a = self.cpu.a;
                let (memory, digit) = if y == 4 {
                    ((a << 4) | (value >> 4), value & 0x0F)
                } else {
                    ((value << 4) | (a & 0x0F), value >> 4)
                };
                let result = (a & 0xF0) | digit;
                self.cpu.a = result;
                self.write(address, memory);
                self.cpu.wz = address.wrapping_add(1);
                self.set_flags((self.cpu.f & CARRY) | sz53p(result));
            }
            _ => {}
        }
    }

    /// LDI, CPI, INI, OUTI (y = 4), their decrementing forms (5) and the repeating forms of
    /// both (6, 7).
    fn block(&mut self, y: u8, z: u8) {
        let step: u16 = if y & 1 == 0 { 1 } else { 0xFFFF };
        let repeats = y >= 6;
        match z {
            0 => {
                let value = self.read(self.cpu.hl());
                self.write(self.cpu.de(), value);
                self.internal(2);
                self.cpu.set_hl(self.cpu.hl().wrapping_add(step));
                self.cpu.set_de(self.cpu.de().wrapping_add(step));
                let bc = self.cpu.bc().wrapping_sub(1);
                self.cpu.set_bc(bc);
                let n = value.wrapping_add(self.cpu.a);
                let more = if bc != 0 { PARITY } else { 0 };
                let kept = self.cpu.f & (SIGN | ZERO | CARRY);
                self.set_flags(kept | (n & X) | ((n << 4) & Y) | more);
                if repeats && bc != 0 {
                    self.repeat_block();
                }
            }
            1 => {
                let value = self.read(self.cpu.hl());
                self.internal(5);
                let a = self.cpu.a;
                let result = a.wrapping_sub(value);
                let half = (a ^ value ^ result) & HALF;
                let n = result.wrapping_sub(half >> 4);
                self.cpu.set_hl(self.cpu.hl().wrapping_add(step));
                self.cpu.wz = self.cpu.wz.wrapping_add(step);
                let bc = self.cpu.bc().wrapping_sub(1);
                self.cpu.set_bc(bc);
                let more = if bc != 0 { PARITY } else { 0 };
                let sz = sz53(result) & !XY;
                let flags = (self.cpu.f & CARRY) | sz | half | (n & X) | ((n << 4) & Y);
                self.set_flags(flags | more | SUBTRACT);
                if repeats && bc != 0 && result != 0 {
                    self.repeat_block();
                }
            }
            2 => {
                self.internal(1);
                let bc = self.cpu.bc();
                let value = self.input(bc);
                self.write(self.cpu.hl(), value);
                self.cpu.wz = bc.wrapping_add(step);
                self.cpu.b = self.cpu.b.wrapping_sub(1);
                self.cpu.set_hl(self.cpu.hl().wrapping_add(step));
                let k = u16::from(value) + u16::from(self.cpu.c.wrapping_add(step as u8));
                self.block_io_end(value, k, repeats);
            }
            _ => {
                self.internal(1);
                let value = self.read(self.cpu.hl());
                self.cpu.b = self.cpu.b.wrapping_sub(1);
                let bc = self.cpu.bc();
                self.output(bc, value);
                self.cpu.wz = bc.wrapping_add(step);
                self.cpu.set_hl(self.cpu.hl().wrapping_add(step));
                let k = u16::from(value) + u16::from(self.cpu.l);
                self.block_io_end(value, k, repeats);
            }
        }
    }

    /// The flags of INI, IND, OUTI and OUTD and the repeat of their repeating forms: `k` is
    /// the transferred byte plus C+1, C-1 or L, as each instruction defines it.
    fn block_io_end(&mut self, value: u8, k: u16, repeats: bool) {
        let b = self.cpu.b;
        let carry = if k > 0xFF { HALF | CARRY } else { 0 };
        let negative = (value >> 6) & SUBTRACT;
        let flags = sz53(b) | negative | carry | parity((k as u8 & 7) ^ b);
        self.set_flags(flags);
        if !repeats || b == 0 {
            return;
        }
        self.repeat_block();
        // A repeating step also changes P/V and H by the B the next iteration will see.
        let flags = self.cpu.f;
        let (parity_source, half) = if flags & CARRY == 0 {
            (b, flags & HALF)
        } else if value & 0x80 != 0 {
            (b.wrapping_sub(1), if b & 0x0F == 0x00 { HALF } else { 0 })
        } else {
            (b.wrapping_add(1), if b & 0x0F == 0x0F { HALF } else { 0 })
        };
        let flip = parity(parity_source & 7) ^ PARITY;
        self.set_flags(((flags & !HALF) | half) ^ flip);
    }

    /// Takes a repeating block instruction back to its start, with X and Y from the high
    /// byte of its address.
    fn repeat_block(&mut self) {
        self.internal(5);
        self.cpu.pc = self.cpu.pc.wrapping_sub(2);
        self.cpu.wz = self.cpu.pc.wrapping_add(1);
        let xy = (self.cpu.pc >> 8) as u8 & XY;
        self.set_flags((self.cpu.f & !XY) | xy);
    }
}
