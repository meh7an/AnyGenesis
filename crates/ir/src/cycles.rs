//! Static cycle times from the timing tables in Section 8 of the MC68000 User's Manual
//! (M68000UM, 16-bit instruction execution times), corrected where SingleStepTests' counts
//! differ (M1.1; each correction says so). Every figure includes the instruction fetch, as
//! the manual's do.
//!
//! Times that depend on data are split: the static part here is the least the instruction
//! takes, and the lifter adds the rest with `Op::Cycles` (shift and rotate counts, MULU/MULS
//! source bits, DIVU/DIVS steps, DBcc expiry, Scc on a register, bit numbers in Dn).

use anygenesis_m68k::{Ea, Instruction, Mnemonic, Operand};
use anygenesis_shared::Size;

use crate::op::Cycles;

/// MULU/MULS take 38 + 2n; the lifter charges the 2n.
const MULTIPLY: u16 = 38;

/// Group 2 exception times (Table 8-14), without effective address time. SingleStepTests
/// takes CHK's trap in 38 cycles, not the manual's 40.
const TRAP: u16 = 34;
const CHK_TRAP: u16 = 38;
const ZERO_DIVIDE: u16 = 38;

/// Returns the static time of `instruction` on both paths.
pub(crate) fn estimate(instruction: &Instruction) -> Cycles {
    let cycles = timing(instruction);
    // Writes to SR and MOVE USP fall through unless the privilege violation is taken.
    let privileged = matches!(
        instruction.operands,
        [_, Some(Operand::Sr)] | [Some(Operand::Usp), _] | [_, Some(Operand::Usp)]
    );
    if privileged {
        Cycles {
            taken: TRAP,
            ..cycles
        }
    } else {
        cycles
    }
}

/// Cycles from the start of `instruction` to where the 68000 latches the interrupt level its
/// end acts on: after its operand reads through an effective address, at the start of the
/// second write for MOVE.L to memory, and at its start when it reads no operand. The latch
/// points come from the microcode (US patent 4,325,121, as read by BlastEm's author on the
/// SpritesMind forum, 2017). Only TST and Bcc are checked against the reference so far.
pub(crate) fn latch(instruction: &Instruction) -> u16 {
    let size = instruction.size.unwrap_or(Size::Word);
    let [first, second] = instruction.operands;
    let in_register = |operand| {
        matches!(
            ea(operand),
            None | Some(Ea::DataRegister(_) | Ea::AddressRegister(_))
        )
    };
    match instruction.mnemonic {
        Mnemonic::Move | Mnemonic::Movea if size == Size::Long && !in_register(second) => {
            ea_time(first, size) + 4
        }
        Mnemonic::Move
        | Mnemonic::Movea
        | Mnemonic::Adda
        | Mnemonic::Suba
        | Mnemonic::Cmp
        | Mnemonic::Cmpa => ea_time(first, size),
        // Toward a data register the source is read, else the destination is.
        Mnemonic::Add | Mnemonic::Sub | Mnemonic::And | Mnemonic::Or => match ea(second) {
            Some(Ea::DataRegister(_)) => ea_time(first, size),
            _ => ea_time(second, size),
        },
        Mnemonic::Mulu | Mnemonic::Muls | Mnemonic::Divu | Mnemonic::Divs | Mnemonic::Chk => {
            ea_time(first, Size::Word)
        }
        Mnemonic::Eor | Mnemonic::Addq | Mnemonic::Subq => ea_time(second, size),
        // The immediate is read first, then the destination.
        Mnemonic::Addi
        | Mnemonic::Subi
        | Mnemonic::Andi
        | Mnemonic::Ori
        | Mnemonic::Eori
        | Mnemonic::Cmpi => match second {
            Some(Operand::Ccr | Operand::Sr) => 4,
            _ => long_or(size, 8, 4) + ea_time(second, size),
        },
        Mnemonic::Clr
        | Mnemonic::Neg
        | Mnemonic::Negx
        | Mnemonic::Not
        | Mnemonic::Nbcd
        | Mnemonic::Scc(_)
        | Mnemonic::Tas
        | Mnemonic::Tst => ea_time(first, size),
        // Only the memory form, one operand, reads.
        Mnemonic::Asl
        | Mnemonic::Asr
        | Mnemonic::Lsl
        | Mnemonic::Lsr
        | Mnemonic::Rol
        | Mnemonic::Ror
        | Mnemonic::Roxl
        | Mnemonic::Roxr => match second {
            None => ea_time(first, Size::Word),
            Some(_) => 0,
        },
        // A static bit number is read before the target.
        Mnemonic::Btst | Mnemonic::Bchg | Mnemonic::Bclr | Mnemonic::Bset => {
            let number = match ea(first) {
                Some(Ea::Immediate(_)) => 4,
                _ => 0,
            };
            number + ea_time(second, Size::Byte)
        }
        // The memory forms read both operands.
        Mnemonic::Addx | Mnemonic::Subx | Mnemonic::Abcd | Mnemonic::Sbcd | Mnemonic::Cmpm => {
            2 * ea_time(first, size)
        }
        _ => 0,
    }
}

fn timing(instruction: &Instruction) -> Cycles {
    let size = instruction.size.unwrap_or(Size::Word);
    let [first, second] = instruction.operands;
    let both = |cycles: u16| Cycles {
        not_taken: cycles,
        taken: cycles,
    };
    let paths = |not_taken: u16, taken: u16| Cycles { not_taken, taken };
    match instruction.mnemonic {
        Mnemonic::Move => both(move_(size, first, second)),
        Mnemonic::Movea => both(move_(size, first, second)),
        Mnemonic::Moveq => both(4),
        Mnemonic::Add | Mnemonic::Sub | Mnemonic::And | Mnemonic::Or => {
            both(standard(size, first, second))
        }
        Mnemonic::Adda | Mnemonic::Suba => {
            let base = match size {
                Size::Long if register_or_immediate(first) => 8,
                Size::Long => 6,
                Size::Byte | Size::Word => 8,
            };
            both(base + ea_time(first, size))
        }
        Mnemonic::Cmp => {
            let base = if size == Size::Long { 6 } else { 4 };
            both(base + ea_time(first, size))
        }
        Mnemonic::Cmpa => both(6 + ea_time(first, size)),
        Mnemonic::Eor => both(match second {
            Some(Operand::Ea(Ea::DataRegister(_))) => long_or(size, 8, 4),
            _ => long_or(size, 12, 8) + ea_time(second, size),
        }),
        Mnemonic::Addi | Mnemonic::Subi | Mnemonic::Andi | Mnemonic::Ori | Mnemonic::Eori => {
            both(immediate(size, second))
        }
        Mnemonic::Cmpi => both(match second {
            Some(Operand::Ea(Ea::DataRegister(_))) => long_or(size, 14, 8),
            _ => long_or(size, 12, 8) + ea_time(second, size),
        }),
        Mnemonic::Addq | Mnemonic::Subq => both(match second {
            Some(Operand::Ea(Ea::DataRegister(_))) => long_or(size, 8, 4),
            // The manual lists ADDQ.w #,An as 4; SingleStepTests takes 8, as for SUBQ.
            Some(Operand::Ea(Ea::AddressRegister(_))) => 8,
            _ => long_or(size, 12, 8) + ea_time(second, size),
        }),
        Mnemonic::Clr | Mnemonic::Neg | Mnemonic::Negx | Mnemonic::Not => both(single(
            size,
            first,
            long_or(size, 6, 4),
            long_or(size, 12, 8),
        )),
        Mnemonic::Nbcd => both(single(size, first, 6, 8)),
        // Scc on a register takes 4 when false; the lifter adds 2 when true.
        Mnemonic::Scc(_) => both(single(size, first, 4, 8)),
        // The manual gives 14 + EA on memory; SingleStepTests takes 10 + EA.
        Mnemonic::Tas => both(single(size, first, 4, 10)),
        Mnemonic::Tst => both(4 + ea_time(first, size)),
        Mnemonic::Asl
        | Mnemonic::Asr
        | Mnemonic::Lsl
        | Mnemonic::Lsr
        | Mnemonic::Rol
        | Mnemonic::Ror
        | Mnemonic::Roxl
        | Mnemonic::Roxr => both(shift(size, first, second)),
        Mnemonic::Btst | Mnemonic::Bchg | Mnemonic::Bclr | Mnemonic::Bset => {
            both(bit(instruction.mnemonic, first, second))
        }
        Mnemonic::Bcc(_) => match size {
            Size::Byte => paths(8, 10),
            Size::Word | Size::Long => paths(12, 10),
        },
        Mnemonic::Bra => both(10),
        Mnemonic::Bsr => both(18),
        // Falling through is either the condition holding (12) or the counter expiring
        // (14); the lifter adds the 2.
        Mnemonic::Dbcc(_) => paths(12, 10),
        Mnemonic::Jmp => both(control(first)),
        Mnemonic::Jsr => both(control(first) + 8),
        Mnemonic::Lea => both(load_address(first)),
        Mnemonic::Pea => both(load_address(first) + 8),
        Mnemonic::Movem => both(move_multiple(size, first, second)),
        Mnemonic::Movep => both(long_or(size, 24, 16)),
        Mnemonic::Addx | Mnemonic::Subx => both(match first {
            Some(Operand::Ea(Ea::DataRegister(_))) => long_or(size, 8, 4),
            _ => long_or(size, 30, 18),
        }),
        Mnemonic::Cmpm => both(long_or(size, 20, 12)),
        Mnemonic::Abcd | Mnemonic::Sbcd => both(match first {
            Some(Operand::Ea(Ea::DataRegister(_))) => 6,
            _ => 18,
        }),
        Mnemonic::Mulu | Mnemonic::Muls => both(MULTIPLY + ea_time(first, Size::Word)),
        // The whole division time depends on data (`BinaryOp::DivuTime`, `DivsTime`).
        Mnemonic::Divu | Mnemonic::Divs => paths(
            ea_time(first, Size::Word),
            ZERO_DIVIDE + ea_time(first, Size::Word),
        ),
        Mnemonic::Chk => paths(
            10 + ea_time(first, Size::Word),
            CHK_TRAP + ea_time(first, Size::Word),
        ),
        Mnemonic::Exg => both(6),
        Mnemonic::Ext | Mnemonic::Swap | Mnemonic::Nop => both(4),
        Mnemonic::Link => both(16),
        Mnemonic::Unlk => both(12),
        Mnemonic::Rts => both(16),
        Mnemonic::Rtr | Mnemonic::Rte => both(20),
        Mnemonic::Stop => both(4),
        Mnemonic::Reset => paths(132, TRAP),
        Mnemonic::Trapv => paths(4, TRAP),
        Mnemonic::Trap | Mnemonic::Illegal | Mnemonic::LineA | Mnemonic::LineF => both(TRAP),
    }
}

/// Returns `long` for a long operation, else `short`.
fn long_or(size: Size, long: u16, short: u16) -> u16 {
    if size == Size::Long { long } else { short }
}

fn ea(operand: Option<Operand>) -> Option<Ea> {
    match operand {
        Some(Operand::Ea(ea)) => Some(ea),
        _ => None,
    }
}

fn register_or_immediate(operand: Option<Operand>) -> bool {
    matches!(
        ea(operand),
        Some(Ea::DataRegister(_) | Ea::AddressRegister(_) | Ea::Immediate(_))
    )
}

/// Effective address calculation time, operand fetch included (Table 8-1).
fn ea_time(operand: Option<Operand>, size: Size) -> u16 {
    let Some(ea) = ea(operand) else {
        return 0;
    };
    let short = match ea {
        Ea::DataRegister(_) | Ea::AddressRegister(_) => return 0,
        Ea::Indirect(_) | Ea::PostIncrement(_) | Ea::Immediate(_) => 4,
        Ea::PreDecrement(_) => 6,
        Ea::Displacement { .. } | Ea::AbsoluteShort(_) | Ea::PcDisplacement(_) => 8,
        Ea::Index { .. } | Ea::PcIndex { .. } => 10,
        Ea::AbsoluteLong(_) => 12,
    };
    long_or(size, short + 4, short)
}

/// The cost of writing a destination: Tables 8-2 and 8-3 minus their source column. A
/// predecrement costs no more than `(An)` here.
fn write_time(operand: Option<Operand>, size: Size) -> u16 {
    let short = match ea(operand) {
        None | Some(Ea::DataRegister(_) | Ea::AddressRegister(_)) => return 0,
        Some(Ea::Indirect(_) | Ea::PostIncrement(_) | Ea::PreDecrement(_)) => 4,
        Some(Ea::Displacement { .. } | Ea::AbsoluteShort(_)) => 8,
        Some(Ea::Index { .. }) => 10,
        Some(Ea::AbsoluteLong(_)) => 12,
        Some(Ea::PcDisplacement(_) | Ea::PcIndex { .. } | Ea::Immediate(_)) => 0,
    };
    long_or(size, short + 4, short)
}

/// MOVE and MOVEA (Tables 8-2, 8-3), and the special-register moves (Table 8-12).
fn move_(size: Size, source: Option<Operand>, destination: Option<Operand>) -> u16 {
    match (source, destination) {
        (Some(Operand::Usp), _) | (_, Some(Operand::Usp)) => 4,
        (Some(Operand::Sr), Some(Operand::Ea(Ea::DataRegister(_)))) => 6,
        (Some(Operand::Sr), _) => 8 + ea_time(destination, Size::Word),
        (_, Some(Operand::Ccr | Operand::Sr)) => 12 + ea_time(source, Size::Word),
        _ => 4 + ea_time(source, size) + write_time(destination, size),
    }
}

/// ADD, SUB, AND and OR (Table 8-4), in either direction.
fn standard(size: Size, source: Option<Operand>, destination: Option<Operand>) -> u16 {
    if let Some(Ea::DataRegister(_)) = ea(destination) {
        let base = match size {
            Size::Long if register_or_immediate(source) => 8,
            Size::Long => 6,
            Size::Byte | Size::Word => 4,
        };
        base + ea_time(source, size)
    } else {
        long_or(size, 12, 8) + ea_time(destination, size)
    }
}

/// ADDI, SUBI, ANDI, ORI and EORI (Table 8-5), including the CCR and SR forms (Table 8-12).
fn immediate(size: Size, destination: Option<Operand>) -> u16 {
    match destination {
        Some(Operand::Ccr | Operand::Sr) => 20,
        // The manual lists ANDI.l #,Dn as 14; SingleStepTests takes 16, as for the others.
        Some(Operand::Ea(Ea::DataRegister(_))) => long_or(size, 16, 8),
        _ => long_or(size, 20, 12) + ea_time(destination, size),
    }
}

/// Single-operand instructions (Table 8-6): `register` on Dn, `memory` plus EA time else.
fn single(size: Size, operand: Option<Operand>, register: u16, memory: u16) -> u16 {
    match ea(operand) {
        Some(Ea::DataRegister(_)) => register,
        _ => memory + ea_time(operand, size),
    }
}

/// Shifts and rotates (Table 8-7): 2 cycles per step on a register, one step in memory. A
/// register count's steps are the lifter's.
fn shift(size: Size, first: Option<Operand>, second: Option<Operand>) -> u16 {
    let steps = match (first, second) {
        (Some(Operand::Quick(count)), Some(_)) => count.unsigned_abs().into(),
        (Some(Operand::Ea(Ea::DataRegister(_))), Some(_)) => 0,
        _ => return 8 + ea_time(first, Size::Word),
    };
    long_or(size, 8, 6) + 2 * steps
}

/// Bit operations (Table 8-8): dynamic with the bit number in Dn, static with it immediate.
/// On a data register, changing a bit of the upper word costs 2 cycles more (SingleStepTests;
/// the manual gives only the maximum): static numbers are charged here, the lifter charges
/// numbers from Dn.
fn bit(mnemonic: Mnemonic, number: Option<Operand>, target: Option<Operand>) -> u16 {
    let dynamic = matches!(ea(number), Some(Ea::DataRegister(_)));
    let (register, memory) = match (mnemonic, dynamic) {
        (Mnemonic::Btst, true) => (6, 4),
        (Mnemonic::Btst, false) => (10, 8),
        (Mnemonic::Bclr, true) => (8, 8),
        (Mnemonic::Bclr, false) => (12, 12),
        (_, true) => (6, 8),
        (_, false) => (10, 12),
    };
    match (ea(target), ea(number)) {
        (Some(Ea::DataRegister(_)), Some(Ea::Immediate(value)))
            if mnemonic != Mnemonic::Btst && value & 31 >= 16 =>
        {
            register + 2
        }
        (Some(Ea::DataRegister(_)), _) => register,
        // BTST Dn,#imm: SingleStepTests takes 10, 2 more than the memory row gives.
        (Some(Ea::Immediate(_)), _) => 10,
        _ => memory + ea_time(target, Size::Byte),
    }
}

/// JMP times per control mode (Table 8-10); JSR takes 8 more in every mode.
fn control(operand: Option<Operand>) -> u16 {
    match ea(operand) {
        Some(Ea::Displacement { .. } | Ea::AbsoluteShort(_) | Ea::PcDisplacement(_)) => 10,
        Some(Ea::Index { .. } | Ea::PcIndex { .. }) => 14,
        Some(Ea::AbsoluteLong(_)) => 12,
        _ => 8,
    }
}

/// LEA times per control mode (Table 8-10); PEA takes 8 more in every mode.
fn load_address(operand: Option<Operand>) -> u16 {
    match ea(operand) {
        Some(Ea::Indirect(_)) => 4,
        Some(Ea::Displacement { .. } | Ea::AbsoluteShort(_) | Ea::PcDisplacement(_)) => 8,
        _ => 12,
    }
}

/// MOVEM (Table 8-10): a base per mode plus 4 cycles per word moved.
fn move_multiple(size: Size, first: Option<Operand>, second: Option<Operand>) -> u16 {
    let (list, memory, to_registers) = match (first, second) {
        (Some(Operand::RegisterList(list)), memory) => (list, memory, false),
        (memory, Some(Operand::RegisterList(list))) => (list, memory, true),
        _ => return 0,
    };
    // At most 16 registers, so the count fits.
    let count = list.0.count_ones() as u16;
    let base = match (ea(memory), to_registers) {
        (Some(Ea::Indirect(_) | Ea::PostIncrement(_)), true) => 12,
        (Some(Ea::Displacement { .. } | Ea::AbsoluteShort(_) | Ea::PcDisplacement(_)), true) => 16,
        (Some(Ea::Index { .. } | Ea::PcIndex { .. }), true) => 18,
        (Some(Ea::AbsoluteLong(_)), true) => 20,
        (Some(Ea::Displacement { .. } | Ea::AbsoluteShort(_)), false) => 12,
        (Some(Ea::Index { .. }), false) => 14,
        (Some(Ea::AbsoluteLong(_)), false) => 16,
        _ => 8,
    };
    base + count * long_or(size, 8, 4)
}

#[cfg(test)]
mod tests {
    use anygenesis_m68k::decode;

    use super::*;

    fn cycles_of(words: &[u16]) -> Cycles {
        estimate(&decode(words).expect("decodes"))
    }

    fn latch_of(words: &[u16]) -> u16 {
        latch(&decode(words).expect("decodes"))
    }

    #[test]
    fn test_latch_follows_the_operand_reads() {
        // tst.b ($F62A).w; bne.s; nop; move.l d0,(a0); add.w d1,(a0); addi.w #1,(a0)
        assert_eq!(latch_of(&[0x4A38, 0xF62A]), 8);
        assert_eq!(latch_of(&[0x66FA]), 0);
        assert_eq!(latch_of(&[0x4E71]), 0);
        assert_eq!(latch_of(&[0x2080]), 4);
        assert_eq!(latch_of(&[0xD350]), 4);
        assert_eq!(latch_of(&[0x0650, 0x0001]), 8);
    }

    #[test]
    fn test_estimate_moves_match_tables_8_2_and_8_3() {
        // move.w d0,d1; move.l (a0),(a1); move.l (xxx).l,(xxx).l; move.w d0,-(a1)
        assert_eq!(cycles_of(&[0x3200]).not_taken, 4);
        assert_eq!(cycles_of(&[0x2290]).not_taken, 20);
        assert_eq!(cycles_of(&[0x23F9, 0, 0, 0, 0]).not_taken, 36);
        assert_eq!(cycles_of(&[0x3300]).not_taken, 8);
    }

    #[test]
    fn test_estimate_standard_long_register_source_adds_two() {
        // add.l d0,d1 is 8; add.l (a0),d1 is 6 + 8; add.w d1,(a0) is 8 + 4.
        assert_eq!(cycles_of(&[0xD280]).not_taken, 8);
        assert_eq!(cycles_of(&[0xD290]).not_taken, 14);
        assert_eq!(cycles_of(&[0xD350]).not_taken, 12);
    }

    #[test]
    fn test_estimate_branches_have_two_paths() {
        // beq.s, beq.w, dbf
        assert_eq!(
            cycles_of(&[0x6702]),
            Cycles {
                not_taken: 8,
                taken: 10
            }
        );
        assert_eq!(
            cycles_of(&[0x6700, 0x0010]),
            Cycles {
                not_taken: 12,
                taken: 10
            }
        );
        assert_eq!(
            cycles_of(&[0x51C8, 0xFFFE]),
            Cycles {
                not_taken: 12,
                taken: 10
            }
        );
    }

    #[test]
    fn test_estimate_control_modes_follow_table_8_10() {
        // jsr (a0); lea (d16,a0),a1; pea (xxx).l; movem.l d0-d3,-(a7)
        assert_eq!(cycles_of(&[0x4E90]).not_taken, 16);
        assert_eq!(cycles_of(&[0x43E8, 0x0010]).not_taken, 8);
        assert_eq!(cycles_of(&[0x4879, 0, 0]).not_taken, 20);
        assert_eq!(cycles_of(&[0x48E7, 0xF000]).not_taken, 8 + 4 * 8);
    }

    #[test]
    fn test_estimate_traps_charge_exception_time() {
        // trap #0; divu (a0),d0 by zero; chk (a0),d0
        assert_eq!(cycles_of(&[0x4E40]).taken, 34);
        assert_eq!(cycles_of(&[0x80D0]).taken, 38 + 4);
        assert_eq!(cycles_of(&[0x4190]).taken, 38 + 4);
    }

    #[test]
    fn test_estimate_bit_on_register_upper_word_costs_two_more() {
        // bset #3,d0; bset #20,d0; btst #20,d0; bset d1,d0 (number from data)
        assert_eq!(cycles_of(&[0x08C0, 3]).not_taken, 10);
        assert_eq!(cycles_of(&[0x08C0, 20]).not_taken, 12);
        assert_eq!(cycles_of(&[0x0800, 20]).not_taken, 10);
        assert_eq!(cycles_of(&[0x03C0]).not_taken, 6);
    }
}
