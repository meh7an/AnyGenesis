//! The IR interpreter: runs lifted instructions against a [`Cpu`] and a [`Bus`].
//!
//! It is the executable meaning of the IR, proven against SingleStepTests, and the runtime's
//! fallback for code the recompiler did not translate. Every pure primitive is evaluated
//! through [`crate::eval`], the same definition the lifter folds constants with.

use anygenesis_m68k::{DecodeError, MAX_WORDS, decode};
use anygenesis_shared::{Address, Bus, Size};

use crate::{
    eval,
    exception::exception_entry,
    lift::{lift, vector},
    op::{Body, Control, Expr, Flag, Lifted, Op, Reg, ReturnKind, SR_MASK, SR_SUPERVISOR, Value},
};

/// Cycles the illegal-instruction and privilege-violation exceptions take (Table 8-14 of the
/// 68000 manual). `Lifted::cycles` has one taken path, which for RTE and STOP is their
/// normal exit, so a privilege violation is charged here instead.
const TRAP_CYCLES: u16 = 34;

/// The architectural state of a 68000.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cpu {
    pub d: [u32; 8],
    /// A0-A7; A7 is the active stack pointer (SSP in supervisor mode, USP in user mode).
    pub a: [u32; 8],
    /// The stack pointer of the other mode: USP while in supervisor mode, SSP in user mode.
    pub inactive_sp: u32,
    /// The address of the next instruction.
    pub pc: Address,
    /// T, S and the interrupt mask; the flags live in `flags`.
    system: u16,
    /// X, N, Z, V, C, indexed by [`flag_index`].
    flags: [bool; 5],
}

impl Cpu {
    /// Returns a CPU with all registers zero and the status register `sr`, whose S bit
    /// decides which of `usp` and `ssp` becomes A7.
    pub fn new(sr: u16, usp: u32, ssp: u32) -> Self {
        let mut cpu = Self::default();
        cpu.load_sr(sr);
        let (active, inactive) = if cpu.supervisor() {
            (ssp, usp)
        } else {
            (usp, ssp)
        };
        cpu.a[7] = active;
        cpu.inactive_sp = inactive;
        cpu
    }

    /// Returns the status register, flags included.
    pub fn sr(&self) -> u16 {
        Flag::ALL
            .into_iter()
            .filter(|&flag| self.flag(flag))
            .fold(self.system, |sr, flag| sr | 1 << flag.bit())
    }

    /// Writes the status register; when S changes, A7 and the inactive stack pointer swap.
    pub fn set_sr(&mut self, sr: u16) {
        let was_supervisor = self.supervisor();
        self.load_sr(sr);
        if self.supervisor() != was_supervisor {
            std::mem::swap(&mut self.a[7], &mut self.inactive_sp);
        }
    }

    /// Returns whether the CPU is in supervisor mode.
    pub fn supervisor(&self) -> bool {
        u32::from(self.system) & SR_SUPERVISOR != 0
    }

    /// Returns the user stack pointer.
    pub fn usp(&self) -> u32 {
        if self.supervisor() {
            self.inactive_sp
        } else {
            self.a[7]
        }
    }

    /// Returns the supervisor stack pointer.
    pub fn ssp(&self) -> u32 {
        if self.supervisor() {
            self.a[7]
        } else {
            self.inactive_sp
        }
    }

    /// Returns one condition code flag.
    pub fn flag(&self, flag: Flag) -> bool {
        self.flags[flag_index(flag)]
    }

    /// Sets one condition code flag.
    pub fn set_flag(&mut self, flag: Flag, value: bool) {
        self.flags[flag_index(flag)] = value;
    }

    /// Writes the status register without swapping stack pointers.
    fn load_sr(&mut self, sr: u16) {
        // SR_MASK covers bits 0-15 only, so the masked value fits a u16.
        let sr = (u32::from(sr) & SR_MASK) as u16;
        self.system = sr & 0xFF00;
        self.write_ccr(sr);
    }

    fn write_ccr(&mut self, ccr: u16) {
        for flag in Flag::ALL {
            self.set_flag(flag, ccr >> flag.bit() & 1 != 0);
        }
    }

    fn register(&self, reg: Reg) -> u32 {
        match reg {
            Reg::Data(n) => self.d[usize::from(n & 7)],
            Reg::Address(n) => self.a[usize::from(n & 7)],
            Reg::InactiveSp => self.inactive_sp,
        }
    }

    fn write_register(&mut self, reg: Reg, size: Size, value: u32) {
        match reg {
            Reg::Data(n) => {
                let slot = &mut self.d[usize::from(n & 7)];
                *slot = (*slot & !size.mask()) | (value & size.mask());
            }
            Reg::Address(n) => self.a[usize::from(n & 7)] = value,
            Reg::InactiveSp => self.inactive_sp = value,
        }
    }
}

const fn flag_index(flag: Flag) -> usize {
    match flag {
        Flag::X => 0,
        Flag::N => 1,
        Flag::Z => 2,
        Flag::V => 3,
        Flag::C => 4,
    }
}

/// Where control went when an instruction did not fall through, with computed addresses
/// resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Transfer {
    /// A jump to a known or computed address (BRA, Bcc, DBcc, JMP).
    Jump(Address),
    /// A subroutine call whose return address is already pushed (BSR, JSR).
    Call {
        target: Address,
        return_address: Address,
    },
    /// A return to the address popped from the stack (RTS, RTR, RTE).
    Return { kind: ReturnKind, target: Address },
    /// An exception was taken; `pc` is the address pushed in the frame.
    Exception { vector: u8, pc: Address },
    /// STOP: the CPU waits for an interrupt, then continues at `resume`.
    Stop { resume: Address },
}

/// The result of running one body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    /// `None` when the body fell through.
    pub transfer: Option<Transfer>,
    /// Whether RESET asserted the reset line of the external devices.
    pub reset_devices: bool,
}

/// One instruction executed by [`Interpreter::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// The instruction's address.
    pub address: Address,
    /// The instruction's cycle estimate on the path it took, exception entry included.
    pub cycles: u16,
    pub outcome: Outcome,
}

/// Runs IR bodies; it owns the temporary storage so repeated runs do not allocate.
#[derive(Debug, Default)]
pub struct Interpreter {
    temps: Vec<u32>,
}

impl Interpreter {
    /// Fetches, decodes, lifts and executes the instruction at `cpu.pc`, taking any
    /// exception it raises, and leaves `cpu.pc` at the next instruction to run.
    ///
    /// Opcode words are read through `bus` one at a time, so no word past the instruction
    /// is touched. An opcode the 68000 does not define takes the illegal-instruction
    /// exception. A STOP leaves `cpu.pc` at the instruction after it; waiting for the
    /// interrupt is the caller's job.
    pub fn step<B: Bus + ?Sized>(&mut self, cpu: &mut Cpu, bus: &mut B) -> Step {
        let address = cpu.pc;
        match fetch(address, bus) {
            Ok(lifted) => self.run(&lifted, cpu, bus),
            Err(_) => {
                let outcome = self.raise(cpu, bus, vector::ILLEGAL, address);
                Step {
                    address,
                    cycles: TRAP_CYCLES,
                    outcome,
                }
            }
        }
    }

    /// Executes an instruction lifted earlier as [`Interpreter::step`] would, taking any
    /// exception it raises, and leaves `cpu.pc` at the next instruction to run. The body
    /// may differ from the lifter's (flag pruning); `lifted.address` should equal `cpu.pc`.
    pub fn run<B: Bus + ?Sized>(&mut self, lifted: &Lifted, cpu: &mut Cpu, bus: &mut B) -> Step {
        let address = lifted.address;
        let mut outcome = self.execute(&lifted.body, cpu, bus);
        cpu.pc = match outcome.transfer {
            None => lifted.next(),
            Some(Transfer::Jump(target) | Transfer::Call { target, .. }) => target,
            Some(Transfer::Return { target, .. }) => target,
            Some(Transfer::Stop { resume }) => resume,
            Some(Transfer::Exception { vector, pc }) => {
                let entry = self.raise(cpu, bus, vector, pc);
                outcome.reset_devices |= entry.reset_devices;
                cpu.pc
            }
        };
        let cycles = match outcome.transfer {
            None => lifted.cycles.not_taken,
            Some(Transfer::Exception {
                vector: vector::PRIVILEGE,
                ..
            }) => TRAP_CYCLES,
            Some(_) => lifted.cycles.taken,
        };
        Step {
            address,
            cycles,
            outcome,
        }
    }

    /// Executes `body` and returns how control left it. `cpu.pc` is not changed: the
    /// transfer says where to go.
    pub fn execute<B: Bus + ?Sized>(&mut self, body: &Body, cpu: &mut Cpu, bus: &mut B) -> Outcome {
        self.temps.clear();
        self.temps.resize(body.temps.len(), 0);
        let mut outcome = Outcome::default();
        for op in &body.ops {
            match *op {
                Op::Let { dst, expr } => {
                    let value = self.expr(body, cpu, bus, expr);
                    self.temps[dst.index()] = value;
                }
                Op::WriteReg { reg, size, value } => {
                    let value = self.value(value);
                    cpu.write_register(reg, size, value);
                }
                Op::SetFlag { flag, value } => {
                    let value = self.value(value) != 0;
                    cpu.set_flag(flag, value);
                }
                // A Word value fits a u16.
                Op::WriteSr { value } => cpu.set_sr(self.value(value) as u16),
                Op::WriteCcr { value } => cpu.write_ccr(self.value(value) as u16),
                Op::Store {
                    size,
                    address,
                    value,
                } => {
                    let address = Address::new(self.value(address));
                    bus.write(size, address, self.value(value));
                }
                Op::ResetDevices => outcome.reset_devices = true,
                Op::Branch { condition, control } => {
                    if self.value(condition) != 0 {
                        outcome.transfer = Some(self.transfer(control));
                        return outcome;
                    }
                }
                Op::Exit(control) => {
                    outcome.transfer = Some(self.transfer(control));
                    return outcome;
                }
            }
        }
        outcome
    }

    /// Runs the exception entry sequence and points `cpu.pc` at the handler.
    fn raise<B: Bus + ?Sized>(
        &mut self,
        cpu: &mut Cpu,
        bus: &mut B,
        vector: u8,
        pc: Address,
    ) -> Outcome {
        let entry = self.execute(&exception_entry(vector, pc), cpu, bus);
        if let Some(Transfer::Jump(handler)) = entry.transfer {
            cpu.pc = handler;
        }
        Outcome {
            transfer: Some(Transfer::Exception { vector, pc }),
            reset_devices: entry.reset_devices,
        }
    }

    fn value(&self, value: Value) -> u32 {
        match value {
            Value::Temp(temp) => self.temps[temp.index()],
            Value::Const { value, .. } => value,
        }
    }

    fn expr<B: Bus + ?Sized>(&self, body: &Body, cpu: &Cpu, bus: &mut B, expr: Expr) -> u32 {
        match expr {
            Expr::ReadReg { reg, size } => cpu.register(reg) & size.mask(),
            Expr::ReadFlag(flag) => u32::from(cpu.flag(flag)),
            Expr::ReadSr => u32::from(cpu.sr()),
            Expr::Supervisor => u32::from(cpu.supervisor()),
            Expr::Load { size, address } => bus.read(size, Address::new(self.value(address))),
            Expr::Unary { op, value } => eval::unary(op, body.width(value), self.value(value)),
            Expr::Binary { op, left, right } => {
                eval::binary(op, body.width(left), self.value(left), self.value(right))
            }
            Expr::Shift { op, value, count } => {
                eval::shift(op, body.width(value), self.value(value), self.value(count))
            }
            Expr::Convert { op, width, value } => {
                eval::convert(op, body.width(value), width, self.value(value))
            }
            Expr::Select {
                condition,
                if_true,
                if_false,
            } => {
                if self.value(condition) != 0 {
                    self.value(if_true)
                } else {
                    self.value(if_false)
                }
            }
            Expr::Flag {
                op,
                left,
                right,
                result,
            } => u32::from(eval::flag(
                op,
                body.width(left),
                self.value(left),
                self.value(right),
                self.value(result),
            )),
        }
    }

    fn transfer(&self, control: Control) -> Transfer {
        match control {
            Control::Jump(target) => Transfer::Jump(target),
            Control::JumpIndirect(target) => Transfer::Jump(Address::new(self.value(target))),
            Control::Call {
                target,
                return_address,
            } => Transfer::Call {
                target,
                return_address,
            },
            Control::CallIndirect {
                target,
                return_address,
            } => Transfer::Call {
                target: Address::new(self.value(target)),
                return_address,
            },
            Control::Return { kind, target } => Transfer::Return {
                kind,
                target: Address::new(self.value(target)),
            },
            Control::Exception { vector, pc } => Transfer::Exception { vector, pc },
            Control::Stop { resume } => Transfer::Stop { resume },
        }
    }
}

/// Reads the instruction at `address` word by word until the decoder has all it needs,
/// then lifts it.
fn fetch<B: Bus + ?Sized>(address: Address, bus: &mut B) -> Result<Lifted, DecodeError> {
    let mut words = [0; MAX_WORDS];
    let mut fetched = 0;
    let instruction = loop {
        match decode(&words[..fetched]) {
            Err(DecodeError::Truncated { needed, .. }) if needed <= MAX_WORDS => {
                for (index, word) in words.iter_mut().enumerate().take(needed).skip(fetched) {
                    *word = bus.read_word(address.wrapping_add(2 * index as u32));
                }
                fetched = needed;
            }
            result => break result?,
        }
    };
    // Every decoded instruction lifts: the exhaustive lifting test covers all of them.
    Ok(lift(&instruction, address).expect("decoded instructions always lift"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KiB of memory mirrored across the address space.
    struct Memory(Vec<u8>);

    impl Memory {
        fn with_words(address: u32, words: &[u16]) -> Self {
            let mut memory = Self(vec![0; 0x1_0000]);
            for (index, &word) in words.iter().enumerate() {
                memory.write_word(Address::new(address + 2 * index as u32), word);
            }
            memory
        }

        fn slot(address: Address) -> usize {
            (address.get() & 0xFFFF) as usize
        }
    }

    impl Bus for Memory {
        fn read_byte(&mut self, address: Address) -> u8 {
            self.0[Self::slot(address)]
        }

        fn read_word(&mut self, address: Address) -> u16 {
            let slot = Self::slot(address);
            u16::from_be_bytes([self.0[slot], self.0[slot + 1]])
        }

        fn write_byte(&mut self, address: Address, value: u8) {
            self.0[Self::slot(address)] = value;
        }

        fn write_word(&mut self, address: Address, value: u16) {
            let slot = Self::slot(address);
            self.0[slot..slot + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    #[test]
    fn test_step_moveq_sets_register_flags_and_pc() {
        let mut memory = Memory::with_words(0x1000, &[0x70FF]);
        let mut cpu = Cpu::new(0x2700, 0, 0x8000);
        cpu.pc = Address::new(0x1000);

        let step = Interpreter::default().step(&mut cpu, &mut memory);

        assert_eq!(cpu.d[0], 0xFFFF_FFFF);
        assert!(cpu.flag(Flag::N));
        assert!(!cpu.flag(Flag::Z));
        assert_eq!(cpu.pc, Address::new(0x1002));
        assert_eq!(step.outcome, Outcome::default());
        assert_eq!(step.cycles, 4);
    }

    #[test]
    fn test_step_trap_from_user_mode_enters_supervisor_with_frame() {
        let mut memory = Memory::with_words(0x1000, &[0x4E41]);
        memory.write_long(Address::new(33 * 4), 0x2000);
        let mut cpu = Cpu::new(0x0004, 0x4000, 0x8000);
        cpu.pc = Address::new(0x1000);

        let step = Interpreter::default().step(&mut cpu, &mut memory);

        assert_eq!(
            step.outcome.transfer,
            Some(Transfer::Exception {
                vector: 33,
                pc: Address::new(0x1002)
            })
        );
        assert_eq!(cpu.pc, Address::new(0x2000));
        assert_eq!(cpu.sr(), 0x2004);
        assert_eq!((cpu.usp(), cpu.ssp()), (0x4000, 0x7FFA));
        assert_eq!(memory.read_word(Address::new(0x7FFA)), 0x0004);
        assert_eq!(memory.read_long(Address::new(0x7FFC)), 0x1002);
        assert_eq!(step.cycles, 34);
    }

    #[test]
    fn test_step_unassigned_opcode_takes_illegal_instruction_exception() {
        // MOVEC exists from the 68010 on; the 68000 does not define it.
        assert!(decode(&[0x4E7B]).is_err());
        let mut memory = Memory::with_words(0x1000, &[0x4E7B]);
        memory.write_long(Address::new(4 * 4), 0x3000);
        let mut cpu = Cpu::new(0x2700, 0, 0x8000);
        cpu.pc = Address::new(0x1000);

        let step = Interpreter::default().step(&mut cpu, &mut memory);

        assert_eq!(
            step.outcome.transfer,
            Some(Transfer::Exception {
                vector: 4,
                pc: Address::new(0x1000)
            })
        );
        assert_eq!(cpu.pc, Address::new(0x3000));
        assert_eq!(memory.read_long(Address::new(0x7FFC)), 0x1000);
    }

    #[test]
    fn test_step_conditional_branch_charges_taken_path() {
        // beq.s *+4 with Z set
        let mut memory = Memory::with_words(0x1000, &[0x6702]);
        let mut cpu = Cpu::new(0x2704, 0, 0x8000);
        cpu.pc = Address::new(0x1000);

        let step = Interpreter::default().step(&mut cpu, &mut memory);

        assert_eq!(cpu.pc, Address::new(0x1004));
        assert_eq!(step.cycles, 10);
    }

    #[test]
    fn test_set_sr_leaving_supervisor_swaps_stack_pointers() {
        let mut cpu = Cpu::new(0x2000, 0x4000, 0x8000);
        assert_eq!(cpu.a[7], 0x8000);

        cpu.set_sr(0x001F);

        assert_eq!(cpu.a[7], 0x4000);
        assert_eq!(cpu.inactive_sp, 0x8000);
        assert_eq!(cpu.sr(), 0x001F);
        assert_eq!((cpu.usp(), cpu.ssp()), (0x4000, 0x8000));
    }
}
