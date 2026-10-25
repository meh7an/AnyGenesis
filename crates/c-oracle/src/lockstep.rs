//! Musashi in lockstep with the IR interpreter: after every instruction both must hold the
//! same registers, RAM bytes written and I/O log.
//!
//! Where Musashi is known to differ from SingleStepTests, which is authoritative (M0.2),
//! the instruction is masked: Musashi is reloaded from the interpreter's state instead of
//! compared, and the mask is counted.

use std::collections::BTreeMap;

use anygenesis_abi::RAM_SIZE;
use anygenesis_ir::Flag;
use anygenesis_m68k::{Ea, Instruction, Mnemonic, Operand};
use anygenesis_oracle::{
    Registers,
    musashi::{AccessSize, Memory, Musashi},
};
use anygenesis_shared::{Address, Bus};

use crate::{
    bus::{Io, SystemBus, Watch},
    reference::Machine,
};

/// The 68000 PC is compared on its 24 address bits, as in M2.2.
const PC_BITS: u32 = Address::MASK;

/// Line F words Musashi runs as the 68040's MOVE16 although it emulates a 68000.
const MOVE16: std::ops::RangeInclusive<u16> = 0xF620..=0xF627;

/// Musashi with its own memory, kept in step with a reference run.
#[derive(Debug)]
pub struct Lockstep {
    core: Musashi,
    ram: Box<[u8; RAM_SIZE]>,
    io: Io,
    /// Entries of the I/O log already compared.
    io_compared: usize,
    /// Masked instructions by reason.
    pub masked: BTreeMap<&'static str, u64>,
    /// Instructions compared.
    pub compared: u64,
}

impl Lockstep {
    /// Takes the Musashi core; it stays locked while the lockstep lives.
    pub fn new() -> Self {
        Self {
            core: Musashi::lock(),
            ram: Box::new([0; RAM_SIZE]),
            io: Io::new(0),
            io_compared: 0,
            masked: BTreeMap::new(),
            compared: 0,
        }
    }

    /// Makes Musashi's state a copy of `machine`'s.
    pub fn load(&mut self, machine: &Machine) {
        self.core.set_registers(&registers(machine));
        self.ram.copy_from_slice(&machine.ram[..]);
        self.io = machine.io.clone();
        self.io_compared = self.io.log.len();
    }

    /// Runs the instruction the reference just ran and compares the result with `machine`,
    /// whose last instruction `watch` saw.
    ///
    /// # Errors
    /// Returns the first difference.
    pub fn step(
        &mut self,
        rom: &[u8],
        instruction: &Instruction,
        machine: &Machine,
        watch: &Watch,
    ) -> Result<(), String> {
        let mut own = Watch::default();
        {
            let mut bus = SystemBus {
                ram: &mut self.ram,
                rom,
                device: &mut self.io,
                watch: Some(&mut own),
            };
            self.core.step_with(&mut bus);
        }
        if let Some(reason) = mask(instruction, machine, watch) {
            *self.masked.entry(reason).or_default() += 1;
            self.load(machine);
            return Ok(());
        }
        self.compared += 1;
        let mut expected = registers(machine);
        let mut actual = self.core.registers();
        expected.pc &= PC_BITS;
        actual.pc &= PC_BITS;
        if expected != actual {
            return Err(format!(
                "registers {actual:x?} against the interpreter's {expected:x?}"
            ));
        }
        for &index in watch.ram_writes.iter().chain(&own.ram_writes) {
            if self.ram[index] != machine.ram[index] {
                return Err(format!(
                    "RAM {:04x} holds {:02x} against the interpreter's {:02x}",
                    index, self.ram[index], machine.ram[index]
                ));
            }
        }
        let theirs = &self.io.log[self.io_compared.min(self.io.log.len())..];
        let ours = &machine.io.log[self.io_compared.min(machine.io.log.len())..];
        if theirs != ours {
            return Err(format!(
                "I/O {theirs:x?} against the interpreter's {ours:x?}"
            ));
        }
        self.io_compared = self.io.log.len();
        // A return to a popped value with a nonzero top byte leaves it in Musashi's PC, and
        // the next push would store it; the IR keeps 24 bits (M2.2), so Musashi is reloaded.
        if self.core.registers().pc & !PC_BITS != 0 {
            *self.masked.entry("PC upper byte").or_default() += 1;
            self.load(machine);
        }
        Ok(())
    }
}

impl Default for Lockstep {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns why Musashi cannot be compared after `instruction`, if it cannot.
fn mask(instruction: &Instruction, machine: &Machine, watch: &Watch) -> Option<&'static str> {
    // Musashi emulates address errors, which the CPU fidelity bar leaves out.
    if watch.odd_access {
        return Some("odd address (address error)");
    }
    match instruction.mnemonic {
        Mnemonic::Abcd | Mnemonic::Sbcd | Mnemonic::Nbcd => Some("BCD"),
        Mnemonic::Chk => Some("CHK"),
        // The IR sets V on overflow and clears it otherwise.
        Mnemonic::Divu | Mnemonic::Divs if machine.cpu.flag(Flag::V) => Some("DIVx overflow"),
        Mnemonic::Link if instruction.operands[0] == Some(Operand::Ea(Ea::AddressRegister(7))) => {
            Some("LINK A7")
        }
        Mnemonic::LineF if MOVE16.contains(&instruction.opcode()) => Some("line F MOVE16"),
        _ => None,
    }
}

fn registers(machine: &Machine) -> Registers {
    let cpu = &machine.cpu;
    let mut a = [0; 7];
    a.copy_from_slice(&cpu.a[..7]);
    Registers {
        d: cpu.d,
        a,
        usp: cpu.usp(),
        ssp: cpu.ssp(),
        sr: cpu.sr().into(),
        pc: cpu.pc.get(),
    }
}

impl Memory for SystemBus<'_> {
    fn read(&mut self, address: u32, size: AccessSize) -> u16 {
        let address = Address::new(address);
        match size {
            AccessSize::Byte => self.read_byte(address).into(),
            AccessSize::Word => self.read_word(address),
        }
    }

    fn write(&mut self, address: u32, size: AccessSize, value: u16) {
        let address = Address::new(address);
        match size {
            // Musashi passes a byte in the low half.
            AccessSize::Byte => self.write_byte(address, value as u8),
            AccessSize::Word => self.write_word(address, value),
        }
    }
}
