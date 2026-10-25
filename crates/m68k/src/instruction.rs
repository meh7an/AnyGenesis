use anygenesis_shared::{Address, Size};

use crate::Ea;

/// The longest 68000 instruction in words: `move.l #imm,(xxx).l`.
pub const MAX_WORDS: usize = 5;

/// A decoded 68000 instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instruction {
    /// The operation, one per Motorola mnemonic.
    pub mnemonic: Mnemonic,
    /// The operation size from the 68000 manual's attributes, `None` for unsized operations.
    /// Branches carry their displacement size instead: `Byte` for `.s`, `Word` for `.w`.
    pub size: Option<Size>,
    /// The operands in Motorola source order (source first); unused slots are `None` and
    /// always trail the used ones.
    pub operands: [Option<Operand>; 2],
    words: [u16; MAX_WORDS],
    length: u8,
}

impl Instruction {
    pub(crate) fn new(
        mnemonic: Mnemonic,
        size: Option<Size>,
        operands: [Option<Operand>; 2],
        words: &[u16],
    ) -> Self {
        let mut stored = [0; MAX_WORDS];
        stored[..words.len()].copy_from_slice(words);
        Self {
            mnemonic,
            size,
            operands,
            words: stored,
            // At most MAX_WORDS words, so the byte count fits easily.
            length: (words.len() * 2) as u8,
        }
    }

    /// Returns the instruction length in bytes, opcode word included (2 to 10).
    pub const fn length(&self) -> u32 {
        self.length as u32
    }

    /// Returns the opcode word.
    pub const fn opcode(&self) -> u16 {
        self.words[0]
    }

    /// Returns the raw words, opcode first, then the extension words in fetch order.
    pub fn words(&self) -> &[u16] {
        &self.words[..usize::from(self.length / 2)]
    }

    /// Returns the target of a branch (Bcc, BRA, BSR, DBcc) at `address`, or `None` when
    /// the instruction has no branch displacement.
    pub fn branch_target(&self, address: Address) -> Option<Address> {
        self.operands
            .iter()
            .flatten()
            .find_map(|operand| match operand {
                // The displacement is relative to the word after the opcode.
                Operand::Branch(displacement) => {
                    Some(address.wrapping_add(2).offset(i32::from(*displacement)))
                }
                _ => None,
            })
    }
}

/// An instruction operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operand {
    /// An effective address, registers included.
    Ea(Ea),
    /// Data held in the opcode word itself: 1 to 8 for ADDQ, SUBQ and shift counts
    /// (an encoded 0 means 8), -128 to 127 for MOVEQ.
    Quick(i8),
    /// A TRAP vector number, 0 to 15.
    Vector(u8),
    /// A branch displacement relative to the address after the opcode word.
    Branch(i16),
    /// A MOVEM register set.
    RegisterList(RegisterList),
    /// The condition code register.
    Ccr,
    /// The status register.
    Sr,
    /// The user stack pointer.
    Usp,
}

/// A MOVEM register set: bit 0 is D0, bit 7 D7, bit 8 A0, bit 15 A7.
///
/// The encoding reverses the mask for `-(An)`; the decoder undoes that, so this order holds
/// for every addressing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegisterList(pub u16);

/// A condition, as Bcc, DBcc and Scc encode it in bits 11-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Condition {
    True,
    False,
    Higher,
    LowerOrSame,
    CarryClear,
    CarrySet,
    NotEqual,
    Equal,
    OverflowClear,
    OverflowSet,
    Plus,
    Minus,
    GreaterOrEqual,
    Less,
    Greater,
    LessOrEqual,
}

impl Condition {
    /// Decodes a 4-bit condition field.
    pub const fn from_bits(bits: u16) -> Self {
        match bits & 0xF {
            0 => Self::True,
            1 => Self::False,
            2 => Self::Higher,
            3 => Self::LowerOrSame,
            4 => Self::CarryClear,
            5 => Self::CarrySet,
            6 => Self::NotEqual,
            7 => Self::Equal,
            8 => Self::OverflowClear,
            9 => Self::OverflowSet,
            10 => Self::Plus,
            11 => Self::Minus,
            12 => Self::GreaterOrEqual,
            13 => Self::Less,
            14 => Self::Greater,
            _ => Self::LessOrEqual,
        }
    }
}

/// A 68000 operation, one variant per Motorola mnemonic.
///
/// Special-register forms keep their base mnemonic (`move` to SR is [`Mnemonic::Move`] with
/// an [`Operand::Sr`] destination). Bcc never carries `True` or `False`: those encodings are
/// BRA and BSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mnemonic {
    Abcd,
    Add,
    Adda,
    Addi,
    Addq,
    Addx,
    And,
    Andi,
    Asl,
    Asr,
    Bcc(Condition),
    Bchg,
    Bclr,
    Bra,
    Bset,
    Bsr,
    Btst,
    Chk,
    Clr,
    Cmp,
    Cmpa,
    Cmpi,
    Cmpm,
    Dbcc(Condition),
    Divs,
    Divu,
    Eor,
    Eori,
    Exg,
    Ext,
    /// The ILLEGAL instruction, 0x4AFC: the illegal-instruction exception on purpose.
    Illegal,
    Jmp,
    Jsr,
    Lea,
    /// Any 0xAxxx word: the line 1010 emulator exception.
    LineA,
    /// Any 0xFxxx word: the line 1111 emulator exception.
    LineF,
    Link,
    Lsl,
    Lsr,
    Move,
    Movea,
    Movem,
    Movep,
    Moveq,
    Muls,
    Mulu,
    Nbcd,
    Neg,
    Negx,
    Nop,
    Not,
    Or,
    Ori,
    Pea,
    Reset,
    Rol,
    Ror,
    Roxl,
    Roxr,
    Rte,
    Rtr,
    Rts,
    Sbcd,
    Scc(Condition),
    Stop,
    Sub,
    Suba,
    Subi,
    Subq,
    Subx,
    Swap,
    Tas,
    Trap,
    Trapv,
    Tst,
    Unlk,
}
