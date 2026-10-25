//! Table tests per instruction group: mnemonic, size, operands and length.

use anygenesis_m68k::{
    Condition, DecodeError, Ea, IndexRegister, IndexSize, Instruction, Mnemonic, Operand, Register,
    RegisterList, decode, decode_at,
};
use anygenesis_shared::{Address, Size};

/// Words, then the expected mnemonic, size and operands; the length is the word count.
type Row = (&'static [u16], Mnemonic, Option<Size>, [Option<Operand>; 2]);

const B: Option<Size> = Some(Size::Byte);
const W: Option<Size> = Some(Size::Word);
const L: Option<Size> = Some(Size::Long);
const NONE: [Option<Operand>; 2] = [None, None];

fn ea(ea: Ea) -> Option<Operand> {
    Some(Operand::Ea(ea))
}

fn d(register: u8) -> Option<Operand> {
    ea(Ea::DataRegister(register))
}

fn a(register: u8) -> Option<Operand> {
    ea(Ea::AddressRegister(register))
}

fn imm(value: u32) -> Option<Operand> {
    ea(Ea::Immediate(value))
}

fn quick(value: i8) -> Option<Operand> {
    Some(Operand::Quick(value))
}

fn branch(displacement: i16) -> Option<Operand> {
    Some(Operand::Branch(displacement))
}

fn decoded(words: &[u16]) -> Instruction {
    decode(words).unwrap_or_else(|error| panic!("{words:04X?}: {error}"))
}

fn check_rows(rows: &[Row]) {
    for (words, mnemonic, size, operands) in rows {
        let instruction = decoded(words);
        assert_eq!(
            (
                instruction.mnemonic,
                instruction.size,
                instruction.operands,
                instruction.length()
            ),
            (*mnemonic, *size, *operands, words.len() as u32 * 2),
            "{words:04X?}"
        );
        assert_eq!(instruction.words(), *words, "{words:04X?}");
    }
}

fn check_invalid(opcodes: &[u16]) {
    for &opcode in opcodes {
        // Zero extension words, so an invalid opcode never fails for lack of them.
        let result = decode(&[opcode, 0, 0, 0, 0]);
        assert_eq!(
            result,
            Err(DecodeError::InvalidOpcode { opcode }),
            "{opcode:04X}"
        );
    }
}

#[test]
fn test_decode_line_0_immediate_and_bit_ops_decode() {
    check_rows(&[
        // ori #$12,ccr: a byte immediate sits in the low byte of its word.
        (
            &[0x003C, 0xFF12],
            Mnemonic::Ori,
            B,
            [imm(0x12), Some(Operand::Ccr)],
        ),
        (
            &[0x027C, 0xF8FF],
            Mnemonic::Andi,
            W,
            [imm(0xF8FF), Some(Operand::Sr)],
        ),
        (
            &[0x0A3C, 0x0001],
            Mnemonic::Eori,
            B,
            [imm(1), Some(Operand::Ccr)],
        ),
        (
            &[0x0290, 0x1234, 0x5678],
            Mnemonic::Andi,
            L,
            [imm(0x1234_5678), ea(Ea::Indirect(0))],
        ),
        (&[0x0440, 0x0010], Mnemonic::Subi, W, [imm(0x10), d(0)]),
        (
            &[0x0618, 0x0001],
            Mnemonic::Addi,
            B,
            [imm(1), ea(Ea::PostIncrement(0))],
        ),
        (&[0x0C80, 0x0000, 0x0001], Mnemonic::Cmpi, L, [imm(1), d(0)]),
        // Bit ops are long on Dn, byte on memory; the bit number is a byte immediate.
        (&[0x0800, 0xFF03], Mnemonic::Btst, L, [imm(3), d(0)]),
        (
            &[0x08D1, 0x0007],
            Mnemonic::Bset,
            B,
            [imm(7), ea(Ea::Indirect(1))],
        ),
        (
            &[0x083A, 0x0001, 0x0010],
            Mnemonic::Btst,
            B,
            [imm(1), ea(Ea::PcDisplacement(16))],
        ),
        (&[0x0303], Mnemonic::Btst, L, [d(1), d(3)]),
        (&[0x033C, 0x00AA], Mnemonic::Btst, B, [d(1), imm(0xAA)]),
        (&[0x0790], Mnemonic::Bclr, B, [d(3), ea(Ea::Indirect(0))]),
        (&[0x05C3], Mnemonic::Bset, L, [d(2), d(3)]),
    ]);
    check_invalid(&[
        0x00BC, // ori.l to an immediate: there is no long CCR/SR form
        0x083C, // btst #n,#imm
        0x087A, // bchg #n,(d16,pc): not alterable
        0x0E10, // moves is a 68010 instruction
        0x0C3A, // cmpi to (d16,pc) is 68020
        0x0048, // ori.w to An
    ]);
}

#[test]
fn test_decode_movep_shares_bit_op_encoding_decodes() {
    check_rows(&[
        (
            &[0x0548, 0x0010],
            Mnemonic::Movep,
            L,
            [
                ea(Ea::Displacement {
                    register: 0,
                    displacement: 16,
                }),
                d(2),
            ],
        ),
        (
            &[0x0589, 0xFFFE],
            Mnemonic::Movep,
            W,
            [
                d(2),
                ea(Ea::Displacement {
                    register: 1,
                    displacement: -2,
                }),
            ],
        ),
        // Same opmode with a data register is a dynamic bit operation.
        (&[0x0580], Mnemonic::Bclr, L, [d(2), d(0)]),
    ]);
}

#[test]
fn test_decode_move_own_size_encoding_decodes() {
    check_rows(&[
        (&[0x1200], Mnemonic::Move, B, [d(0), d(1)]),
        (&[0x3200], Mnemonic::Move, W, [d(0), d(1)]),
        (&[0x2200], Mnemonic::Move, L, [d(0), d(1)]),
        (
            &[0x23FC, 0x1122, 0x3344, 0x00FF, 0x0000],
            Mnemonic::Move,
            L,
            [imm(0x1122_3344), ea(Ea::AbsoluteLong(0x00FF_0000))],
        ),
        (
            &[0x31FC, 0x8000, 0xF600],
            Mnemonic::Move,
            W,
            [imm(0x8000), ea(Ea::AbsoluteShort(0xF600))],
        ),
        (&[0x3449], Mnemonic::Movea, W, [a(1), a(2)]),
        (&[0x2448], Mnemonic::Movea, L, [a(0), a(2)]),
    ]);
    check_invalid(&[
        0x1008, // move.b A0,D0
        0x1040, // movea.b
        0x15C0, // move to (d16,pc)
        0x11FD, // move.b with source mode 7, register 5
    ]);
}

#[test]
fn test_decode_line_4_misc_decodes() {
    check_rows(&[
        (&[0x4AFC], Mnemonic::Illegal, None, NONE),
        (
            &[0x43E8, 0x0008],
            Mnemonic::Lea,
            L,
            [
                ea(Ea::Displacement {
                    register: 0,
                    displacement: 8,
                }),
                a(1),
            ],
        ),
        (&[0x4181], Mnemonic::Chk, W, [d(1), d(0)]),
        (&[0x4000], Mnemonic::Negx, B, [d(0), None]),
        (&[0x4250], Mnemonic::Clr, W, [ea(Ea::Indirect(0)), None]),
        (&[0x4481], Mnemonic::Neg, L, [d(1), None]),
        (&[0x4602], Mnemonic::Not, B, [d(2), None]),
        (&[0x40C0], Mnemonic::Move, W, [Some(Operand::Sr), d(0)]),
        (&[0x44C0], Mnemonic::Move, W, [d(0), Some(Operand::Ccr)]),
        (
            &[0x46D0],
            Mnemonic::Move,
            W,
            [ea(Ea::Indirect(0)), Some(Operand::Sr)],
        ),
        (&[0x4800], Mnemonic::Nbcd, B, [d(0), None]),
        (&[0x4842], Mnemonic::Swap, W, [d(2), None]),
        (&[0x4850], Mnemonic::Pea, L, [ea(Ea::Indirect(0)), None]),
        (&[0x4883], Mnemonic::Ext, W, [d(3), None]),
        (&[0x48C3], Mnemonic::Ext, L, [d(3), None]),
        (&[0x4AD0], Mnemonic::Tas, B, [ea(Ea::Indirect(0)), None]),
        (&[0x4A50], Mnemonic::Tst, W, [ea(Ea::Indirect(0)), None]),
        (
            &[0x4E4F],
            Mnemonic::Trap,
            None,
            [Some(Operand::Vector(15)), None],
        ),
        (&[0x4E56, 0xFFF8], Mnemonic::Link, W, [a(6), imm(0xFFF8)]),
        (&[0x4E5E], Mnemonic::Unlk, None, [a(6), None]),
        (&[0x4E60], Mnemonic::Move, L, [a(0), Some(Operand::Usp)]),
        (&[0x4E69], Mnemonic::Move, L, [Some(Operand::Usp), a(1)]),
        (&[0x4E70], Mnemonic::Reset, None, NONE),
        (&[0x4E71], Mnemonic::Nop, None, NONE),
        (&[0x4E72, 0x2700], Mnemonic::Stop, None, [imm(0x2700), None]),
        (&[0x4E73], Mnemonic::Rte, None, NONE),
        (&[0x4E75], Mnemonic::Rts, None, NONE),
        (&[0x4E76], Mnemonic::Trapv, None, NONE),
        (&[0x4E77], Mnemonic::Rtr, None, NONE),
        (&[0x4ED0], Mnemonic::Jmp, None, [ea(Ea::Indirect(0)), None]),
        (
            &[0x4EBA, 0x0010],
            Mnemonic::Jsr,
            None,
            [ea(Ea::PcDisplacement(16)), None],
        ),
    ]);
    check_invalid(&[
        0x41C0, // lea D0
        0x42C0, // move from ccr is a 68010 instruction
        0x4A88, // tst.l A0
        0x4ED8, // jmp (A0)+
        0x4E74, // rtd is a 68010 instruction
        0x4E7A, // movec is a 68010 instruction
        0x4848, // bkpt is a 68010 instruction
        0x4C00, // mull is a 68020 instruction
    ]);
}

#[test]
fn test_decode_movem_predecrement_mask_is_reversed() {
    check_rows(&[
        // movem.l d0-d1/a6,-(sp): encoded with A7 in bit 0, normalized to D0 in bit 0.
        (
            &[0x48E7, 0xC002],
            Mnemonic::Movem,
            L,
            [
                Some(Operand::RegisterList(RegisterList(0x4003))),
                ea(Ea::PreDecrement(7)),
            ],
        ),
        (
            &[0x4C9F, 0x0101],
            Mnemonic::Movem,
            W,
            [
                ea(Ea::PostIncrement(7)),
                Some(Operand::RegisterList(RegisterList(0x0101))),
            ],
        ),
        (
            &[0x48D0, 0x0003],
            Mnemonic::Movem,
            L,
            [
                Some(Operand::RegisterList(RegisterList(0x0003))),
                ea(Ea::Indirect(0)),
            ],
        ),
    ]);
    check_invalid(&[
        0x48DF, // registers to (A7)+
        0x4CE7, // -(A7) to registers
        0x48C8, // An
    ]);
}

#[test]
fn test_decode_line_5_quick_and_conditions_decode() {
    check_rows(&[
        // A quick value of 0 means 8.
        (&[0x5048], Mnemonic::Addq, W, [quick(8), a(0)]),
        (&[0x5780], Mnemonic::Subq, L, [quick(3), d(0)]),
        (
            &[0x51C8, 0xFFFE],
            Mnemonic::Dbcc(Condition::False),
            W,
            [d(0), branch(-2)],
        ),
        (&[0x57C1], Mnemonic::Scc(Condition::Equal), B, [d(1), None]),
        (
            &[0x50D0],
            Mnemonic::Scc(Condition::True),
            B,
            [ea(Ea::Indirect(0)), None],
        ),
    ]);
    check_invalid(&[
        0x5208, // addq.b to An
        0x57FC, // seq #imm (trapcc on the 68020)
        0x50FA, // st (d16,pc)
    ]);
}

#[test]
fn test_decode_branch_displacements_decode() {
    check_rows(&[
        (&[0x6004], Mnemonic::Bra, B, [branch(4), None]),
        // A displacement byte of 0 means a word displacement follows.
        (&[0x6000, 0x0100], Mnemonic::Bra, W, [branch(256), None]),
        // On the 68000, 0xFF is a byte displacement of -1.
        (&[0x60FF], Mnemonic::Bra, B, [branch(-1), None]),
        (&[0x6110], Mnemonic::Bsr, B, [branch(16), None]),
        (
            &[0x67FE],
            Mnemonic::Bcc(Condition::Equal),
            B,
            [branch(-2), None],
        ),
        (
            &[0x6200, 0x8000],
            Mnemonic::Bcc(Condition::Higher),
            W,
            [branch(-32768), None],
        ),
    ]);
}

#[test]
fn test_decode_branch_target_relative_to_word_after_opcode() {
    let address = Address::new(0x1000);

    assert_eq!(
        decoded(&[0x6000, 0xFFFE]).branch_target(address),
        Some(Address::new(0x1000))
    );
    assert_eq!(
        decoded(&[0x60FF]).branch_target(address),
        Some(Address::new(0x1001))
    );
    assert_eq!(
        decoded(&[0x51C8, 0x0010]).branch_target(address),
        Some(Address::new(0x1012))
    );
    assert_eq!(decoded(&[0x4E71]).branch_target(address), None);
}

#[test]
fn test_decode_moveq_sign_extends_data() {
    check_rows(&[
        (&[0x76FF], Mnemonic::Moveq, L, [quick(-1), d(3)]),
        (&[0x707F], Mnemonic::Moveq, L, [quick(127), d(0)]),
    ]);
    check_invalid(&[0x7100]);
}

#[test]
fn test_decode_line_8_and_c_shared_encodings_disambiguate() {
    let predecrement = |register| ea(Ea::PreDecrement(register));
    check_rows(&[
        (&[0x8101], Mnemonic::Sbcd, B, [d(1), d(0)]),
        (
            &[0x8109],
            Mnemonic::Sbcd,
            B,
            [predecrement(1), predecrement(0)],
        ),
        (&[0x8250], Mnemonic::Or, W, [ea(Ea::Indirect(0)), d(1)]),
        (&[0x8310], Mnemonic::Or, B, [d(1), ea(Ea::Indirect(0))]),
        (&[0x80D0], Mnemonic::Divu, W, [ea(Ea::Indirect(0)), d(0)]),
        (&[0x81FC, 0x0003], Mnemonic::Divs, W, [imm(3), d(0)]),
        (&[0xC101], Mnemonic::Abcd, B, [d(1), d(0)]),
        (
            &[0xC109],
            Mnemonic::Abcd,
            B,
            [predecrement(1), predecrement(0)],
        ),
        (&[0xC141], Mnemonic::Exg, L, [d(0), d(1)]),
        (&[0xC149], Mnemonic::Exg, L, [a(0), a(1)]),
        (&[0xC189], Mnemonic::Exg, L, [d(0), a(1)]),
        (&[0xC150], Mnemonic::And, W, [d(0), ea(Ea::Indirect(0))]),
        (&[0xC0C1], Mnemonic::Mulu, W, [d(1), d(0)]),
        (&[0xC1C1], Mnemonic::Muls, W, [d(1), d(0)]),
    ]);
    check_invalid(&[
        0x8181, // or.l D0,D1 in the Dn,<ea> form
        0xC181, // and.l D0,D1 in the Dn,<ea> form
        0x8148, // pack is a 68020 instruction
        0xC1C8, // muls from An
    ]);
}

#[test]
fn test_decode_add_sub_extended_forms_disambiguate() {
    let predecrement = |register| ea(Ea::PreDecrement(register));
    check_rows(&[
        (&[0xD181], Mnemonic::Addx, L, [d(1), d(0)]),
        (
            &[0xD109],
            Mnemonic::Addx,
            B,
            [predecrement(1), predecrement(0)],
        ),
        (&[0xD1C9], Mnemonic::Adda, L, [a(1), a(0)]),
        (&[0xD151], Mnemonic::Add, W, [d(0), ea(Ea::Indirect(1))]),
        (&[0xD048], Mnemonic::Add, W, [a(0), d(0)]),
        (
            &[0x9149],
            Mnemonic::Subx,
            W,
            [predecrement(1), predecrement(0)],
        ),
        (&[0x90C0], Mnemonic::Suba, W, [d(0), a(0)]),
        (&[0x9090], Mnemonic::Sub, L, [ea(Ea::Indirect(0)), d(0)]),
    ]);
    check_invalid(&[
        0xD008, // add.b A0,D0
        0x917A, // sub.w D0,(d16,pc)
    ]);
}

#[test]
fn test_decode_line_b_cmpm_and_eor_disambiguate() {
    check_rows(&[
        (
            &[0xB109],
            Mnemonic::Cmpm,
            B,
            [ea(Ea::PostIncrement(1)), ea(Ea::PostIncrement(0))],
        ),
        (&[0xB340], Mnemonic::Eor, W, [d(1), d(0)]),
        (&[0xB081], Mnemonic::Cmp, L, [d(1), d(0)]),
        (&[0xB2D0], Mnemonic::Cmpa, W, [ea(Ea::Indirect(0)), a(1)]),
        (&[0xB048], Mnemonic::Cmp, W, [a(0), d(0)]),
    ]);
    check_invalid(&[
        0xB37C, // eor to an immediate
        0xB008, // cmp.b A0,D0
    ]);
}

#[test]
fn test_decode_line_e_shifts_decode() {
    check_rows(&[
        (&[0xE140], Mnemonic::Asl, W, [quick(8), d(0)]),
        (&[0xE2AA], Mnemonic::Lsr, L, [d(1), d(2)]),
        (&[0xE21B], Mnemonic::Ror, B, [quick(1), d(3)]),
        (&[0xE5D0], Mnemonic::Roxl, W, [ea(Ea::Indirect(0)), None]),
        (&[0xE0D0], Mnemonic::Asr, W, [ea(Ea::Indirect(0)), None]),
    ]);
    check_invalid(&[
        0xE8C0, // bftst is a 68020 instruction
        0xE0C0, // memory shift on Dn
    ]);
}

#[test]
fn test_decode_line_a_and_f_are_distinct_traps() {
    check_rows(&[
        (&[0xA123], Mnemonic::LineA, None, NONE),
        (&[0xF200], Mnemonic::LineF, None, NONE),
        (&[0xF620], Mnemonic::LineF, None, NONE),
    ]);
    assert_eq!(decoded(&[0xA123]).opcode(), 0xA123);
}

#[test]
fn test_decode_brief_extension_ignores_scale_and_full_format_bits() {
    let index = |register| IndexRegister {
        register,
        size: IndexSize::Long,
    };
    // lea (-2,a0,a3.l),a1 with bits 8-10 (scale and full format on later CPUs) all set.
    check_rows(&[
        (
            &[0x43F0, 0xBFFE],
            Mnemonic::Lea,
            L,
            [
                ea(Ea::Index {
                    register: 0,
                    displacement: -2,
                    index: index(Register::Address(3)),
                }),
                a(1),
            ],
        ),
        (
            &[0x4EFB, 0x0802],
            Mnemonic::Jmp,
            None,
            [
                ea(Ea::PcIndex {
                    displacement: 2,
                    index: index(Register::Data(0)),
                }),
                None,
            ],
        ),
    ]);
}

#[test]
fn test_decode_missing_words_returns_truncated() {
    assert_eq!(
        decode(&[]),
        Err(DecodeError::Truncated {
            needed: 1,
            available: 0
        })
    );
    assert_eq!(
        decode(&[0x4EF9, 0x0000]),
        Err(DecodeError::Truncated {
            needed: 3,
            available: 2
        })
    );
    assert_eq!(
        decode(&[0x0800]),
        Err(DecodeError::Truncated {
            needed: 2,
            available: 1
        })
    );
    // An invalid opcode wins over missing words.
    assert_eq!(
        decode(&[0x4E7A]),
        Err(DecodeError::InvalidOpcode { opcode: 0x4E7A })
    );
}

#[test]
fn test_decode_at_past_end_of_rom_returns_truncated() {
    let rom = [0x4E, 0x71, 0x4E, 0xF9, 0x00];

    assert_eq!(
        decode_at(&rom, 0).map(|instruction| instruction.mnemonic),
        Ok(Mnemonic::Nop)
    );
    assert_eq!(
        decode_at(&rom, 2),
        Err(DecodeError::Truncated {
            needed: 3,
            available: 1
        })
    );
    assert_eq!(
        decode_at(&rom, 6),
        Err(DecodeError::Truncated {
            needed: 1,
            available: 0
        })
    );
    assert_eq!(
        decode_at(&rom, usize::MAX),
        Err(DecodeError::Truncated {
            needed: 1,
            available: 0
        })
    );
}

#[test]
fn test_decode_at_ignores_bytes_after_instruction() {
    let rom = [0x30, 0x3C, 0x12, 0x34, 0xFF, 0xFF];

    let instruction = decode_at(&rom, 0).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(instruction.words(), [0x303C, 0x1234]);
    assert_eq!(instruction.operands, [imm(0x1234), d(0)]);
}
