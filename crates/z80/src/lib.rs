//! A Z80 interpreter (NMOS), one instruction or interrupt acknowledge per [`Z80::step`].
//!
//! Written from the documented behaviour of the Z80, no other emulator's code, and checked
//! against SingleStepTests/z80: the undocumented X and Y flags, MEMPTR (WZ) and Q follow
//! that suite. Interrupt acknowledge, which the suite does not cover, follows the documented
//! timings. Every bus access reports the T-state of its step at which it happens, so a
//! caller can place it on its own clock.

mod alu;
mod execute;

/// The memory and I/O the Z80 sees.
///
/// `at` is the T-state of the current step, counted from 0, at which the read or write
/// strobe of the access falls: the second T-state of a memory cycle and the third of an
/// I/O cycle.
pub trait Bus {
    /// Reads a byte of memory, opcode fetches included.
    fn read(&mut self, address: u16, at: u32) -> u8;

    /// Writes a byte of memory.
    fn write(&mut self, address: u16, value: u8, at: u32);

    /// Reads an I/O port; `port` carries the full 16-bit address the Z80 puts out.
    fn input(&mut self, port: u16, at: u32) -> u8;

    /// Writes an I/O port.
    fn output(&mut self, port: u16, value: u8, at: u32);

    /// The byte the interrupting device places on the data bus when a maskable interrupt
    /// is acknowledged; 0xFF, an undriven bus, by default.
    fn acknowledge(&mut self, at: u32) -> u8 {
        let _ = at;
        0xFF
    }
}

/// The Z80's registers and internal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Z80 {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    /// The alternate register pairs AF', BC', DE' and HL'.
    pub af_: u16,
    pub bc_: u16,
    pub de_: u16,
    pub hl_: u16,
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,
    pub i: u8,
    pub r: u8,
    /// The internal MEMPTR register; BIT n,(HL) shows its high byte in X and Y.
    pub wz: u16,
    /// The interrupt mode, 0, 1 or 2.
    pub im: u8,
    pub iff1: bool,
    pub iff2: bool,
    /// Whether HALT stopped the CPU; it then runs internal no-ops until an interrupt.
    pub halted: bool,
    /// Whether the last instruction was EI, which holds off a maskable interrupt for one
    /// instruction.
    pub ei: bool,
    /// Whether the last instruction was LD A,I or LD A,R, whose P/V an interrupt accepted
    /// right after it clears.
    pub p: bool,
    /// The flags the last instruction wrote, or 0 if it wrote none; SCF and CCF take X and
    /// Y from it.
    pub q: u8,
    /// The level of the maskable interrupt input.
    int_line: bool,
    /// A non-maskable interrupt edge not yet taken.
    nmi_pending: bool,
}

impl Default for Z80 {
    /// The state after a reset; registers the reset leaves undefined are 0.
    fn default() -> Self {
        Self {
            a: 0xFF,
            f: 0xFF,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            af_: 0,
            bc_: 0,
            de_: 0,
            hl_: 0,
            ix: 0,
            iy: 0,
            sp: 0xFFFF,
            pc: 0,
            i: 0,
            r: 0,
            wz: 0,
            im: 0,
            iff1: false,
            iff2: false,
            halted: false,
            ei: false,
            p: false,
            q: 0,
            int_line: false,
            nmi_pending: false,
        }
    }
}

impl Z80 {
    /// Asserts the RESET input: PC, I, R, the interrupt mode and both interrupt flip-flops
    /// clear and AF and SP become 0xFFFF; the other registers keep their values. The
    /// interrupt inputs are left as they are.
    pub fn reset(&mut self) {
        let int_line = self.int_line;
        *self = Self {
            b: self.b,
            c: self.c,
            d: self.d,
            e: self.e,
            h: self.h,
            l: self.l,
            af_: self.af_,
            bc_: self.bc_,
            de_: self.de_,
            hl_: self.hl_,
            ix: self.ix,
            iy: self.iy,
            wz: self.wz,
            int_line,
            ..Self::default()
        };
    }

    /// Sets the level of the maskable interrupt input; it is sampled before every step.
    pub fn set_interrupt_line(&mut self, asserted: bool) {
        self.int_line = asserted;
    }

    /// Signals a falling edge on the NMI input; the next step takes it.
    pub fn request_nmi(&mut self) {
        self.nmi_pending = true;
    }

    /// Executes one instruction, or acknowledges a pending interrupt, and returns the
    /// T-states it took.
    pub fn step<B: Bus + ?Sized>(&mut self, bus: &mut B) -> u32 {
        execute::Step::new(self, bus).run()
    }

    /// AF as a pair.
    pub fn af(&self) -> u16 {
        u16::from_be_bytes([self.a, self.f])
    }

    /// BC as a pair.
    pub fn bc(&self) -> u16 {
        u16::from_be_bytes([self.b, self.c])
    }

    /// DE as a pair.
    pub fn de(&self) -> u16 {
        u16::from_be_bytes([self.d, self.e])
    }

    /// HL as a pair.
    pub fn hl(&self) -> u16 {
        u16::from_be_bytes([self.h, self.l])
    }

    /// Sets AF as a pair.
    pub fn set_af(&mut self, value: u16) {
        [self.a, self.f] = value.to_be_bytes();
    }

    /// Sets BC as a pair.
    pub fn set_bc(&mut self, value: u16) {
        [self.b, self.c] = value.to_be_bytes();
    }

    /// Sets DE as a pair.
    pub fn set_de(&mut self, value: u16) {
        [self.d, self.e] = value.to_be_bytes();
    }

    /// Sets HL as a pair.
    pub fn set_hl(&mut self, value: u16) {
        [self.h, self.l] = value.to_be_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KiB of RAM with no I/O, and a fixed byte for interrupt acknowledges.
    struct Ram {
        memory: Vec<u8>,
        acknowledge: u8,
    }

    impl Ram {
        fn with_program(address: u16, program: &[u8]) -> Self {
            let mut memory = vec![0; 0x1_0000];
            let start = usize::from(address);
            memory[start..start + program.len()].copy_from_slice(program);
            Self {
                memory,
                acknowledge: 0xFF,
            }
        }

        fn word(&self, address: u16) -> u16 {
            let address = usize::from(address);
            u16::from_le_bytes([self.memory[address], self.memory[address + 1]])
        }
    }

    impl Bus for Ram {
        fn read(&mut self, address: u16, _at: u32) -> u8 {
            self.memory[usize::from(address)]
        }

        fn write(&mut self, address: u16, value: u8, _at: u32) {
            self.memory[usize::from(address)] = value;
        }

        fn input(&mut self, _port: u16, _at: u32) -> u8 {
            0xFF
        }

        fn output(&mut self, _port: u16, _value: u8, _at: u32) {}

        fn acknowledge(&mut self, _at: u32) -> u8 {
            self.acknowledge
        }
    }

    const EI: u8 = 0xFB;
    const NOP: u8 = 0x00;
    const HALT: u8 = 0x76;

    fn cpu_at(pc: u16) -> Z80 {
        Z80 {
            pc,
            sp: 0x8000,
            ..Z80::default()
        }
    }

    #[test]
    fn test_im1_interrupt_waits_one_instruction_after_ei() {
        let mut ram = Ram::with_program(0x0100, &[EI, NOP, NOP]);
        let mut cpu = Z80 {
            im: 1,
            ..cpu_at(0x0100)
        };
        cpu.set_interrupt_line(true);

        assert_eq!(cpu.step(&mut ram), 4);
        assert!(cpu.iff1 && cpu.ei);
        assert_eq!(cpu.step(&mut ram), 4, "the instruction after EI runs first");
        assert_eq!(cpu.pc, 0x0102);

        assert_eq!(cpu.step(&mut ram), 13);
        assert_eq!(cpu.pc, 0x0038);
        assert_eq!(cpu.wz, 0x0038);
        assert_eq!(cpu.sp, 0x7FFE);
        assert_eq!(ram.word(0x7FFE), 0x0102);
        assert!(!cpu.iff1 && !cpu.iff2);
    }

    #[test]
    fn test_im2_interrupt_calls_through_vector_table() {
        let mut ram = Ram::with_program(0x0100, &[NOP]);
        ram.acknowledge = 0x20;
        ram.memory[0x4020..0x4022].copy_from_slice(&[0x34, 0x12]);
        let mut cpu = Z80 {
            im: 2,
            i: 0x40,
            iff1: true,
            iff2: true,
            ..cpu_at(0x0100)
        };
        cpu.set_interrupt_line(true);

        assert_eq!(cpu.step(&mut ram), 19);
        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(ram.word(0x7FFE), 0x0100);
    }

    #[test]
    fn test_im0_interrupt_with_undriven_bus_runs_rst_38() {
        let mut ram = Ram::with_program(0x0100, &[NOP]);
        let mut cpu = Z80 {
            iff1: true,
            ..cpu_at(0x0100)
        };
        cpu.set_interrupt_line(true);

        assert_eq!(cpu.step(&mut ram), 13);
        assert_eq!(cpu.pc, 0x0038);
        assert_eq!(ram.word(0x7FFE), 0x0100);
    }

    #[test]
    fn test_interrupt_masked_while_iff1_clear() {
        let mut ram = Ram::with_program(0x0100, &[NOP]);
        let mut cpu = Z80 {
            im: 1,
            ..cpu_at(0x0100)
        };
        cpu.set_interrupt_line(true);

        assert_eq!(cpu.step(&mut ram), 4);
        assert_eq!(cpu.pc, 0x0101);
    }

    #[test]
    fn test_halt_runs_no_ops_until_interrupt_returns_past_it() {
        let mut ram = Ram::with_program(0x0100, &[HALT, NOP]);
        let mut cpu = Z80 {
            im: 1,
            iff1: true,
            iff2: true,
            ..cpu_at(0x0100)
        };

        assert_eq!(cpu.step(&mut ram), 4);
        assert!(cpu.halted);
        for _ in 0..3 {
            assert_eq!(cpu.step(&mut ram), 4);
        }
        assert_eq!(cpu.pc, 0x0101, "a halted CPU does not advance PC");
        assert_eq!(cpu.r, 4, "every halted no-op is an opcode fetch");

        cpu.set_interrupt_line(true);
        assert_eq!(cpu.step(&mut ram), 13);
        assert!(!cpu.halted);
        assert_eq!(ram.word(0x7FFE), 0x0101);
    }

    #[test]
    fn test_nmi_ignores_iff1_and_retn_restores_it() {
        // RETN at the NMI vector.
        let mut ram = Ram::with_program(0x0100, &[NOP]);
        ram.memory[0x0066..0x0068].copy_from_slice(&[0xED, 0x45]);
        let mut cpu = Z80 {
            iff1: true,
            iff2: true,
            ..cpu_at(0x0100)
        };
        cpu.request_nmi();

        assert_eq!(cpu.step(&mut ram), 11);
        assert_eq!(cpu.pc, 0x0066);
        assert!(!cpu.iff1 && cpu.iff2);

        assert_eq!(cpu.step(&mut ram), 14);
        assert_eq!(cpu.pc, 0x0100);
        assert!(cpu.iff1);
    }

    #[test]
    fn test_interrupt_right_after_ld_a_i_clears_parity() {
        // LD A,I twice: the first is interrupted at its end, the second is not.
        let mut ram = Ram::with_program(0x0100, &[0xED, 0x57, NOP, 0xED, 0x57, NOP]);
        let mut cpu = Z80 {
            im: 1,
            iff1: true,
            iff2: true,
            ..cpu_at(0x0100)
        };

        cpu.step(&mut ram);
        assert_ne!(cpu.f & alu::PARITY, 0, "LD A,I copies IFF2 into P/V");
        cpu.set_interrupt_line(true);
        cpu.step(&mut ram);
        assert_eq!(cpu.f & alu::PARITY, 0);

        let mut cpu = Z80 {
            im: 1,
            iff1: true,
            iff2: true,
            ..cpu_at(0x0103)
        };
        cpu.step(&mut ram);
        cpu.step(&mut ram);
        cpu.set_interrupt_line(true);
        cpu.step(&mut ram);
        assert_ne!(
            cpu.f & alu::PARITY,
            0,
            "only the instruction just before counts"
        );
    }

    #[test]
    fn test_reset_clears_control_state_and_keeps_general_registers() {
        let mut cpu = Z80 {
            pc: 0x1234,
            b: 0x56,
            i: 0x78,
            r: 0x9A,
            im: 2,
            iff1: true,
            iff2: true,
            halted: true,
            ..Z80::default()
        };
        cpu.reset();

        assert_eq!((cpu.pc, cpu.i, cpu.r, cpu.im), (0, 0, 0, 0));
        assert!(!cpu.iff1 && !cpu.iff2 && !cpu.halted);
        assert_eq!((cpu.af(), cpu.sp), (0xFFFF, 0xFFFF));
        assert_eq!(cpu.b, 0x56);
    }
}
