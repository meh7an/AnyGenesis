use anygenesis_shared::{Size, read_u16_be};

use crate::{
    Condition, Ea, EaMode, IndexRegister, IndexSize, Instruction, MAX_WORDS, Mnemonic, Operand,
    Register, RegisterList, ea::EaModes,
};

/// Why a word sequence is not a 68000 instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The opcode word is not assigned on the 68000, or it pairs an operation with an
    /// addressing mode or size the operation does not allow.
    #[error("opcode {opcode:#06x} is not a 68000 instruction")]
    InvalidOpcode { opcode: u16 },
    /// The instruction runs past the end of the words given (for a ROM: past its end).
    #[error("instruction needs {needed} words, only {available} available")]
    Truncated { needed: usize, available: usize },
}

/// Decodes the instruction at the start of `words`, opcode word first.
///
/// Words after the instruction are ignored. Validity depends on the opcode word alone, so an
/// invalid opcode is reported even when its extension words would be missing.
///
/// # Errors
/// Returns [`DecodeError::InvalidOpcode`] for an unassigned encoding and
/// [`DecodeError::Truncated`] when `words` ends before the instruction does.
pub fn decode(words: &[u16]) -> Result<Instruction, DecodeError> {
    let Some(&opcode) = words.first() else {
        return Err(DecodeError::Truncated {
            needed: 1,
            available: 0,
        });
    };
    let mut cursor = Cursor { words, position: 1 };
    let (mnemonic, size, operands) =
        decode_opcode(opcode, &mut cursor).ok_or(DecodeError::InvalidOpcode { opcode })?;
    let needed = cursor.position;
    if needed > words.len() {
        return Err(DecodeError::Truncated {
            needed,
            available: words.len(),
        });
    }
    Ok(Instruction::new(mnemonic, size, operands, &words[..needed]))
}

/// Decodes the instruction whose opcode word starts at byte `offset` of `bytes`, big-endian.
///
/// # Errors
/// As [`decode`]; [`DecodeError::Truncated`] counts the whole words left in `bytes`.
pub fn decode_at(bytes: &[u8], offset: usize) -> Result<Instruction, DecodeError> {
    let mut words = [0; MAX_WORDS];
    let mut available = 0;
    for (index, word) in words.iter_mut().enumerate() {
        let Some(Ok(value)) = offset
            .checked_add(index * 2)
            .map(|position| read_u16_be(bytes, position))
        else {
            break;
        };
        *word = value;
        available += 1;
    }
    decode(&words[..available])
}

/// The decoded parts of an instruction, before its length is known.
type Parts = (Mnemonic, Option<Size>, [Option<Operand>; 2]);

/// Fetches extension words in order.
struct Cursor<'a> {
    words: &'a [u16],
    position: usize,
}

impl Cursor<'_> {
    /// Words past the end read as zero; [`decode`] reports them as truncation once the whole
    /// layout, and so the real length, is known.
    fn next(&mut self) -> u16 {
        let word = self.words.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        word
    }

    fn long(&mut self) -> u32 {
        let high = u32::from(self.next());
        (high << 16) | u32::from(self.next())
    }

    fn immediate(&mut self, size: Size) -> u32 {
        match size {
            Size::Byte => u32::from(self.next() & 0xFF),
            Size::Word => u32::from(self.next()),
            Size::Long => self.long(),
        }
    }

    /// Reads a brief extension word; bits 8-10 (scale and full format on later CPUs) are
    /// ignored, as the 68000 does.
    fn brief(&mut self) -> (i8, IndexRegister) {
        let word = self.next();
        let number = register_number(word >> 12);
        let register = if word & 0x8000 != 0 {
            Register::Address(number)
        } else {
            Register::Data(number)
        };
        let size = if word & 0x0800 != 0 {
            IndexSize::Long
        } else {
            IndexSize::Word
        };
        (word as u8 as i8, IndexRegister { register, size })
    }

    /// Decodes an EA from its mode and register fields, or `None` when `allowed` excludes
    /// the mode. `size` sets the width of an immediate.
    fn ea(&mut self, mode: u16, register: u16, size: Size, allowed: EaModes) -> Option<Ea> {
        let mode = EaMode::from_fields(mode, register)?;
        if !allowed.contains(mode) {
            return None;
        }
        let register = register_number(register);
        Some(match mode {
            EaMode::DataRegister => Ea::DataRegister(register),
            EaMode::AddressRegister => Ea::AddressRegister(register),
            EaMode::Indirect => Ea::Indirect(register),
            EaMode::PostIncrement => Ea::PostIncrement(register),
            EaMode::PreDecrement => Ea::PreDecrement(register),
            EaMode::Displacement => Ea::Displacement {
                register,
                displacement: self.next() as i16,
            },
            EaMode::Index => {
                let (displacement, index) = self.brief();
                Ea::Index {
                    register,
                    displacement,
                    index,
                }
            }
            EaMode::AbsoluteShort => Ea::AbsoluteShort(self.next()),
            EaMode::AbsoluteLong => Ea::AbsoluteLong(self.long()),
            EaMode::PcDisplacement => Ea::PcDisplacement(self.next() as i16),
            EaMode::PcIndex => {
                let (displacement, index) = self.brief();
                Ea::PcIndex {
                    displacement,
                    index,
                }
            }
            EaMode::Immediate => Ea::Immediate(self.immediate(size)),
        })
    }

    /// Decodes the EA in bits 5-0 of `opcode`.
    fn source(&mut self, opcode: u16, size: Size, allowed: EaModes) -> Option<Ea> {
        self.ea(opcode >> 3, opcode, size, allowed)
    }
}

fn decode_opcode(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    match opcode >> 12 {
        0x0 => decode_line_0(opcode, cursor),
        0x1..=0x3 => decode_move(opcode, cursor),
        0x4 => decode_line_4(opcode, cursor),
        0x5 => decode_line_5(opcode, cursor),
        0x6 => Some(decode_branch(opcode, cursor)),
        0x7 => decode_moveq(opcode),
        0x8 => decode_line_8(opcode, cursor),
        0x9 => decode_add_sub(
            opcode,
            cursor,
            [Mnemonic::Sub, Mnemonic::Suba, Mnemonic::Subx],
        ),
        0xA => Some((Mnemonic::LineA, None, [None, None])),
        0xB => decode_line_b(opcode, cursor),
        0xC => decode_line_c(opcode, cursor),
        0xD => decode_add_sub(
            opcode,
            cursor,
            [Mnemonic::Add, Mnemonic::Adda, Mnemonic::Addx],
        ),
        0xE => decode_line_e(opcode, cursor),
        _ => Some((Mnemonic::LineF, None, [None, None])),
    }
}

/// Bit operations, MOVEP and the immediate operations.
fn decode_line_0(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    if opcode & 0x0100 != 0 {
        // MOVEP takes the An mode, which no dynamic bit operation allows.
        if (opcode >> 3) & 7 == 1 {
            return Some(decode_movep(opcode, cursor));
        }
        let mnemonic = bit_mnemonic(opcode);
        let allowed = if mnemonic == Mnemonic::Btst {
            EaModes::DATA
        } else {
            EaModes::DATA_ALTERABLE
        };
        let target = cursor.source(opcode, Size::Byte, allowed)?;
        return Some((
            mnemonic,
            Some(bit_size(target)),
            [Some(data_register(opcode >> 9)), Some(Operand::Ea(target))],
        ));
    }
    match (opcode >> 9) & 7 {
        0 => decode_logical_immediate(opcode, cursor, Mnemonic::Ori),
        1 => decode_logical_immediate(opcode, cursor, Mnemonic::Andi),
        2 => decode_arithmetic_immediate(opcode, cursor, Mnemonic::Subi),
        3 => decode_arithmetic_immediate(opcode, cursor, Mnemonic::Addi),
        4 => decode_static_bit(opcode, cursor),
        5 => decode_logical_immediate(opcode, cursor, Mnemonic::Eori),
        6 => decode_arithmetic_immediate(opcode, cursor, Mnemonic::Cmpi),
        _ => None,
    }
}

fn bit_mnemonic(opcode: u16) -> Mnemonic {
    match (opcode >> 6) & 3 {
        0 => Mnemonic::Btst,
        1 => Mnemonic::Bchg,
        2 => Mnemonic::Bclr,
        _ => Mnemonic::Bset,
    }
}

/// Bit operations work on a long word in a data register and on a byte in memory.
fn bit_size(target: Ea) -> Size {
    match target {
        Ea::DataRegister(_) => Size::Long,
        _ => Size::Byte,
    }
}

fn decode_static_bit(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let mnemonic = bit_mnemonic(opcode);
    let allowed = if mnemonic == Mnemonic::Btst {
        EaModes::DATA.without(EaMode::Immediate)
    } else {
        EaModes::DATA_ALTERABLE
    };
    // The bit number word comes before the EA's extension words.
    let number = cursor.immediate(Size::Byte);
    let target = cursor.source(opcode, Size::Byte, allowed)?;
    Some((
        mnemonic,
        Some(bit_size(target)),
        [Some(immediate(number)), Some(Operand::Ea(target))],
    ))
}

fn decode_movep(opcode: u16, cursor: &mut Cursor) -> Parts {
    let size = if opcode & 0x0040 != 0 {
        Size::Long
    } else {
        Size::Word
    };
    let memory = Operand::Ea(Ea::Displacement {
        register: register_number(opcode),
        displacement: cursor.next() as i16,
    });
    let data = data_register(opcode >> 9);
    let operands = if opcode & 0x0080 != 0 {
        [Some(data), Some(memory)]
    } else {
        [Some(memory), Some(data)]
    };
    (Mnemonic::Movep, Some(size), operands)
}

/// ORI, ANDI and EORI, whose immediate EA selects the CCR (byte) or SR (word) form.
fn decode_logical_immediate(opcode: u16, cursor: &mut Cursor, mnemonic: Mnemonic) -> Option<Parts> {
    let size = size_field(opcode >> 6)?;
    if opcode & 0x3F == 0x3C {
        let register = match size {
            Size::Byte => Operand::Ccr,
            Size::Word => Operand::Sr,
            Size::Long => return None,
        };
        let value = cursor.immediate(size);
        return Some((
            mnemonic,
            Some(size),
            [Some(immediate(value)), Some(register)],
        ));
    }
    decode_arithmetic_immediate(opcode, cursor, mnemonic)
}

fn decode_arithmetic_immediate(
    opcode: u16,
    cursor: &mut Cursor,
    mnemonic: Mnemonic,
) -> Option<Parts> {
    let size = size_field(opcode >> 6)?;
    let value = cursor.immediate(size);
    let target = cursor.source(opcode, size, EaModes::DATA_ALTERABLE)?;
    Some((
        mnemonic,
        Some(size),
        [Some(immediate(value)), Some(Operand::Ea(target))],
    ))
}

/// MOVE and MOVEA, with their own size encoding: 1 byte, 3 word, 2 long.
fn decode_move(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let size = match opcode >> 12 {
        1 => Size::Byte,
        3 => Size::Word,
        _ => Size::Long,
    };
    let source_modes = if size == Size::Byte {
        EaModes::DATA
    } else {
        EaModes::ALL
    };
    let source = cursor.source(opcode, size, source_modes)?;
    let mode = (opcode >> 6) & 7;
    if mode == 1 {
        if size == Size::Byte {
            return None;
        }
        return Some((
            Mnemonic::Movea,
            Some(size),
            [
                Some(Operand::Ea(source)),
                Some(address_register(opcode >> 9)),
            ],
        ));
    }
    let destination = cursor.ea(mode, opcode >> 9, size, EaModes::DATA_ALTERABLE)?;
    Some((
        Mnemonic::Move,
        Some(size),
        [Some(Operand::Ea(source)), Some(Operand::Ea(destination))],
    ))
}

fn decode_line_4(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    if opcode == 0x4AFC {
        return Some((Mnemonic::Illegal, None, [None, None]));
    }
    let register = opcode >> 9;
    if opcode & 0x0100 != 0 {
        return match (opcode >> 6) & 7 {
            6 => {
                let bound = cursor.source(opcode, Size::Word, EaModes::DATA)?;
                Some((
                    Mnemonic::Chk,
                    Some(Size::Word),
                    [Some(Operand::Ea(bound)), Some(data_register(register))],
                ))
            }
            7 => {
                let source = cursor.source(opcode, Size::Long, EaModes::CONTROL)?;
                Some((
                    Mnemonic::Lea,
                    Some(Size::Long),
                    [Some(Operand::Ea(source)), Some(address_register(register))],
                ))
            }
            _ => None,
        };
    }
    let is_word_form = (opcode >> 6) & 3 == 3;
    match register & 7 {
        0 if is_word_form => {
            let target = cursor.source(opcode, Size::Word, EaModes::DATA_ALTERABLE)?;
            Some((
                Mnemonic::Move,
                Some(Size::Word),
                [Some(Operand::Sr), Some(Operand::Ea(target))],
            ))
        }
        0 => decode_unary(opcode, cursor, Mnemonic::Negx),
        1 if is_word_form => None,
        1 => decode_unary(opcode, cursor, Mnemonic::Clr),
        2 if is_word_form => decode_move_to(opcode, cursor, Operand::Ccr),
        2 => decode_unary(opcode, cursor, Mnemonic::Neg),
        3 if is_word_form => decode_move_to(opcode, cursor, Operand::Sr),
        3 => decode_unary(opcode, cursor, Mnemonic::Not),
        4 => decode_line_4_group_4(opcode, cursor),
        5 if is_word_form => {
            let target = cursor.source(opcode, Size::Byte, EaModes::DATA_ALTERABLE)?;
            Some((
                Mnemonic::Tas,
                Some(Size::Byte),
                [Some(Operand::Ea(target)), None],
            ))
        }
        5 => {
            let size = size_field(opcode >> 6)?;
            let target = cursor.source(opcode, size, EaModes::DATA_ALTERABLE)?;
            Some((Mnemonic::Tst, Some(size), [Some(Operand::Ea(target)), None]))
        }
        6 if opcode & 0x0080 != 0 => decode_movem(opcode, cursor),
        6 => None,
        _ => decode_line_4_group_7(opcode, cursor),
    }
}

fn decode_unary(opcode: u16, cursor: &mut Cursor, mnemonic: Mnemonic) -> Option<Parts> {
    let size = size_field(opcode >> 6)?;
    let target = cursor.source(opcode, size, EaModes::DATA_ALTERABLE)?;
    Some((mnemonic, Some(size), [Some(Operand::Ea(target)), None]))
}

fn decode_move_to(opcode: u16, cursor: &mut Cursor, register: Operand) -> Option<Parts> {
    let source = cursor.source(opcode, Size::Word, EaModes::DATA)?;
    Some((
        Mnemonic::Move,
        Some(Size::Word),
        [Some(Operand::Ea(source)), Some(register)],
    ))
}

/// 0x4800-0x48FF: NBCD, SWAP, PEA, EXT and MOVEM to memory.
fn decode_line_4_group_4(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    match ((opcode >> 6) & 3, (opcode >> 3) & 7) {
        (0, _) => {
            let target = cursor.source(opcode, Size::Byte, EaModes::DATA_ALTERABLE)?;
            Some((
                Mnemonic::Nbcd,
                Some(Size::Byte),
                [Some(Operand::Ea(target)), None],
            ))
        }
        (1, 0) => Some((
            Mnemonic::Swap,
            Some(Size::Word),
            [Some(data_register(opcode)), None],
        )),
        (1, _) => {
            let source = cursor.source(opcode, Size::Long, EaModes::CONTROL)?;
            Some((
                Mnemonic::Pea,
                Some(Size::Long),
                [Some(Operand::Ea(source)), None],
            ))
        }
        (2, 0) => Some((
            Mnemonic::Ext,
            Some(Size::Word),
            [Some(data_register(opcode)), None],
        )),
        (3, 0) => Some((
            Mnemonic::Ext,
            Some(Size::Long),
            [Some(data_register(opcode)), None],
        )),
        _ => decode_movem(opcode, cursor),
    }
}

/// MOVEM in either direction; bit 10 set means memory to registers.
fn decode_movem(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let size = if opcode & 0x0040 != 0 {
        Size::Long
    } else {
        Size::Word
    };
    let to_memory = opcode & 0x0400 == 0;
    let allowed = if to_memory {
        EaModes::CONTROL_ALTERABLE.with(EaMode::PreDecrement)
    } else {
        EaModes::CONTROL.with(EaMode::PostIncrement)
    };
    let mask = cursor.next();
    let target = cursor.source(opcode, size, allowed)?;
    // For -(An) the mask runs from A7 (bit 0) to D0 (bit 15).
    let list = Operand::RegisterList(RegisterList(match target {
        Ea::PreDecrement(_) => mask.reverse_bits(),
        _ => mask,
    }));
    let memory = Operand::Ea(target);
    let operands = if to_memory {
        [Some(list), Some(memory)]
    } else {
        [Some(memory), Some(list)]
    };
    Some((Mnemonic::Movem, Some(size), operands))
}

/// 0x4E00-0x4EFF: TRAP, LINK, UNLK, MOVE USP, the fixed control words, JSR and JMP.
fn decode_line_4_group_7(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    match (opcode >> 6) & 3 {
        1 => decode_control(opcode, cursor),
        2 => decode_jump(opcode, cursor, Mnemonic::Jsr),
        3 => decode_jump(opcode, cursor, Mnemonic::Jmp),
        _ => None,
    }
}

/// 0x4E40-0x4E7F.
fn decode_control(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let register = address_register(opcode);
    match (opcode >> 3) & 7 {
        0 | 1 => Some((
            Mnemonic::Trap,
            None,
            [Some(Operand::Vector((opcode & 0xF) as u8)), None],
        )),
        2 => {
            let displacement = cursor.immediate(Size::Word);
            Some((
                Mnemonic::Link,
                Some(Size::Word),
                [Some(register), Some(immediate(displacement))],
            ))
        }
        3 => Some((Mnemonic::Unlk, None, [Some(register), None])),
        4 => Some((
            Mnemonic::Move,
            Some(Size::Long),
            [Some(register), Some(Operand::Usp)],
        )),
        5 => Some((
            Mnemonic::Move,
            Some(Size::Long),
            [Some(Operand::Usp), Some(register)],
        )),
        6 => {
            let mnemonic = match opcode & 7 {
                0 => Mnemonic::Reset,
                1 => Mnemonic::Nop,
                2 => {
                    let status = cursor.immediate(Size::Word);
                    return Some((Mnemonic::Stop, None, [Some(immediate(status)), None]));
                }
                3 => Mnemonic::Rte,
                5 => Mnemonic::Rts,
                6 => Mnemonic::Trapv,
                7 => Mnemonic::Rtr,
                // RTD is a 68010 instruction.
                _ => return None,
            };
            Some((mnemonic, None, [None, None]))
        }
        // MOVEC is a 68010 instruction.
        _ => None,
    }
}

fn decode_jump(opcode: u16, cursor: &mut Cursor, mnemonic: Mnemonic) -> Option<Parts> {
    let target = cursor.source(opcode, Size::Long, EaModes::CONTROL)?;
    Some((mnemonic, None, [Some(Operand::Ea(target)), None]))
}

/// ADDQ, SUBQ, Scc and DBcc.
fn decode_line_5(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let Some(size) = size_field(opcode >> 6) else {
        let condition = Condition::from_bits(opcode >> 8);
        if (opcode >> 3) & 7 == 1 {
            let displacement = cursor.next() as i16;
            return Some((
                Mnemonic::Dbcc(condition),
                Some(Size::Word),
                [
                    Some(data_register(opcode)),
                    Some(Operand::Branch(displacement)),
                ],
            ));
        }
        let target = cursor.source(opcode, Size::Byte, EaModes::DATA_ALTERABLE)?;
        return Some((
            Mnemonic::Scc(condition),
            Some(Size::Byte),
            [Some(Operand::Ea(target)), None],
        ));
    };
    let allowed = if size == Size::Byte {
        EaModes::DATA_ALTERABLE
    } else {
        EaModes::ALTERABLE
    };
    let target = cursor.source(opcode, size, allowed)?;
    let mnemonic = if opcode & 0x0100 != 0 {
        Mnemonic::Subq
    } else {
        Mnemonic::Addq
    };
    Some((
        mnemonic,
        Some(size),
        [Some(quick(opcode >> 9)), Some(Operand::Ea(target))],
    ))
}

/// BRA, BSR and Bcc. A displacement byte of 0 means a word displacement follows; on the 68000
/// 0xFF is just -1 (later CPUs take it as a long displacement).
fn decode_branch(opcode: u16, cursor: &mut Cursor) -> Parts {
    let mnemonic = match (opcode >> 8) & 0xF {
        0 => Mnemonic::Bra,
        1 => Mnemonic::Bsr,
        bits => Mnemonic::Bcc(Condition::from_bits(bits)),
    };
    let (size, displacement) = match opcode as u8 {
        0 => (Size::Word, cursor.next() as i16),
        byte => (Size::Byte, i16::from(byte as i8)),
    };
    (
        mnemonic,
        Some(size),
        [Some(Operand::Branch(displacement)), None],
    )
}

fn decode_moveq(opcode: u16) -> Option<Parts> {
    if opcode & 0x0100 != 0 {
        return None;
    }
    Some((
        Mnemonic::Moveq,
        Some(Size::Long),
        [
            Some(Operand::Quick(opcode as u8 as i8)),
            Some(data_register(opcode >> 9)),
        ],
    ))
}

/// OR, DIVU, DIVS and SBCD.
fn decode_line_8(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    match (opcode >> 6) & 7 {
        3 => decode_word_arithmetic(opcode, cursor, Mnemonic::Divu),
        7 => decode_word_arithmetic(opcode, cursor, Mnemonic::Divs),
        _ if opcode & 0x01F0 == 0x0100 => Some(decode_bcd(opcode, Mnemonic::Sbcd)),
        _ => decode_logical(opcode, cursor, Mnemonic::Or),
    }
}

/// AND, MULU, MULS, ABCD and EXG.
fn decode_line_c(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let (rx, ry) = (opcode >> 9, opcode);
    match (opcode >> 6) & 7 {
        3 => decode_word_arithmetic(opcode, cursor, Mnemonic::Mulu),
        7 => decode_word_arithmetic(opcode, cursor, Mnemonic::Muls),
        _ if opcode & 0x01F0 == 0x0100 => Some(decode_bcd(opcode, Mnemonic::Abcd)),
        _ => {
            let registers = match opcode & 0x01F8 {
                0x0140 => [data_register(rx), data_register(ry)],
                0x0148 => [address_register(rx), address_register(ry)],
                0x0188 => [data_register(rx), address_register(ry)],
                _ => return decode_logical(opcode, cursor, Mnemonic::And),
            };
            Some((Mnemonic::Exg, Some(Size::Long), registers.map(Some)))
        }
    }
}

/// DIVU, DIVS, MULU and MULS: word operand from a data EA into Dn.
fn decode_word_arithmetic(opcode: u16, cursor: &mut Cursor, mnemonic: Mnemonic) -> Option<Parts> {
    let source = cursor.source(opcode, Size::Word, EaModes::DATA)?;
    Some((
        mnemonic,
        Some(Size::Word),
        [Some(Operand::Ea(source)), Some(data_register(opcode >> 9))],
    ))
}

/// AND and OR: `<ea>,Dn` from a data EA, or `Dn,<ea>` into memory.
fn decode_logical(opcode: u16, cursor: &mut Cursor, mnemonic: Mnemonic) -> Option<Parts> {
    let size = size_field(opcode >> 6)?;
    let register = data_register(opcode >> 9);
    if opcode & 0x0100 == 0 {
        let source = cursor.source(opcode, size, EaModes::DATA)?;
        return Some((
            mnemonic,
            Some(size),
            [Some(Operand::Ea(source)), Some(register)],
        ));
    }
    let target = cursor.source(opcode, size, EaModes::MEMORY_ALTERABLE)?;
    Some((
        mnemonic,
        Some(size),
        [Some(register), Some(Operand::Ea(target))],
    ))
}

/// ABCD and SBCD: `Dy,Dx` or `-(Ay),-(Ax)`.
fn decode_bcd(opcode: u16, mnemonic: Mnemonic) -> Parts {
    (mnemonic, Some(Size::Byte), register_pair(opcode).map(Some))
}

/// ADDX and SUBX (`Dy,Dx` or `-(Ay),-(Ax)`), ABCD and SBCD share this pair layout.
fn register_pair(opcode: u16) -> [Operand; 2] {
    let (rx, ry) = (register_number(opcode >> 9), register_number(opcode));
    if opcode & 0x0008 != 0 {
        [
            Operand::Ea(Ea::PreDecrement(ry)),
            Operand::Ea(Ea::PreDecrement(rx)),
        ]
    } else {
        [
            Operand::Ea(Ea::DataRegister(ry)),
            Operand::Ea(Ea::DataRegister(rx)),
        ]
    }
}

/// ADD/ADDA/ADDX or SUB/SUBA/SUBX, given in that order.
fn decode_add_sub(opcode: u16, cursor: &mut Cursor, mnemonics: [Mnemonic; 3]) -> Option<Parts> {
    let [plain, address, extended] = mnemonics;
    let register = opcode >> 9;
    let Some(size) = size_field(opcode >> 6) else {
        return decode_address_arithmetic(opcode, cursor, address);
    };
    if opcode & 0x0100 == 0 {
        let allowed = if size == Size::Byte {
            EaModes::DATA
        } else {
            EaModes::ALL
        };
        let source = cursor.source(opcode, size, allowed)?;
        return Some((
            plain,
            Some(size),
            [Some(Operand::Ea(source)), Some(data_register(register))],
        ));
    }
    // Dn,<ea> allows memory only, so its Dn and An modes encode ADDX/SUBX.
    if (opcode >> 4) & 3 == 0 {
        return Some((extended, Some(size), register_pair(opcode).map(Some)));
    }
    let target = cursor.source(opcode, size, EaModes::MEMORY_ALTERABLE)?;
    Some((
        plain,
        Some(size),
        [Some(data_register(register)), Some(Operand::Ea(target))],
    ))
}

/// ADDA, SUBA and CMPA: bit 8 selects word or long.
fn decode_address_arithmetic(
    opcode: u16,
    cursor: &mut Cursor,
    mnemonic: Mnemonic,
) -> Option<Parts> {
    let size = if opcode & 0x0100 != 0 {
        Size::Long
    } else {
        Size::Word
    };
    let source = cursor.source(opcode, size, EaModes::ALL)?;
    Some((
        mnemonic,
        Some(size),
        [
            Some(Operand::Ea(source)),
            Some(address_register(opcode >> 9)),
        ],
    ))
}

/// CMP, CMPA, CMPM and EOR.
fn decode_line_b(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let register = opcode >> 9;
    let Some(size) = size_field(opcode >> 6) else {
        return decode_address_arithmetic(opcode, cursor, Mnemonic::Cmpa);
    };
    if opcode & 0x0100 == 0 {
        let allowed = if size == Size::Byte {
            EaModes::DATA
        } else {
            EaModes::ALL
        };
        let source = cursor.source(opcode, size, allowed)?;
        return Some((
            Mnemonic::Cmp,
            Some(size),
            [Some(Operand::Ea(source)), Some(data_register(register))],
        ));
    }
    // EOR does not allow An, so that mode encodes CMPM.
    if (opcode >> 3) & 7 == 1 {
        return Some((
            Mnemonic::Cmpm,
            Some(size),
            [
                Some(Operand::Ea(Ea::PostIncrement(register_number(opcode)))),
                Some(Operand::Ea(Ea::PostIncrement(register_number(register)))),
            ],
        ));
    }
    let target = cursor.source(opcode, size, EaModes::DATA_ALTERABLE)?;
    Some((
        Mnemonic::Eor,
        Some(size),
        [Some(data_register(register)), Some(Operand::Ea(target))],
    ))
}

/// Shifts and rotates, on a register or (one bit, word sized) on memory.
fn decode_line_e(opcode: u16, cursor: &mut Cursor) -> Option<Parts> {
    let left = opcode & 0x0100 != 0;
    let Some(size) = size_field(opcode >> 6) else {
        // Bit 11 set selects the 68020 bit-field instructions.
        if opcode & 0x0800 != 0 {
            return None;
        }
        let target = cursor.source(opcode, Size::Word, EaModes::MEMORY_ALTERABLE)?;
        return Some((
            shift_mnemonic(opcode >> 9, left),
            Some(Size::Word),
            [Some(Operand::Ea(target)), None],
        ));
    };
    let count = if opcode & 0x0020 != 0 {
        data_register(opcode >> 9)
    } else {
        quick(opcode >> 9)
    };
    Some((
        shift_mnemonic(opcode >> 3, left),
        Some(size),
        [Some(count), Some(data_register(opcode))],
    ))
}

fn shift_mnemonic(kind: u16, left: bool) -> Mnemonic {
    match (kind & 3, left) {
        (0, false) => Mnemonic::Asr,
        (0, true) => Mnemonic::Asl,
        (1, false) => Mnemonic::Lsr,
        (1, true) => Mnemonic::Lsl,
        (2, false) => Mnemonic::Roxr,
        (2, true) => Mnemonic::Roxl,
        (_, false) => Mnemonic::Ror,
        (_, true) => Mnemonic::Rol,
    }
}

/// The common size field: 0 byte, 1 word, 2 long; 3 selects another instruction.
fn size_field(bits: u16) -> Option<Size> {
    match bits & 3 {
        0 => Some(Size::Byte),
        1 => Some(Size::Word),
        2 => Some(Size::Long),
        _ => None,
    }
}

/// A 3-bit quick field, where 0 encodes 8.
fn quick(bits: u16) -> Operand {
    match bits & 7 {
        0 => Operand::Quick(8),
        value => Operand::Quick(value as i8),
    }
}

fn register_number(bits: u16) -> u8 {
    (bits & 7) as u8
}

fn data_register(bits: u16) -> Operand {
    Operand::Ea(Ea::DataRegister(register_number(bits)))
}

fn address_register(bits: u16) -> Operand {
    Operand::Ea(Ea::AddressRegister(register_number(bits)))
}

fn immediate(value: u32) -> Operand {
    Operand::Ea(Ea::Immediate(value))
}
