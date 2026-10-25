//! Instruction text in two dialects: Motorola syntax as Musashi's disassembler prints it, and
//! the syntax of the AS assembler that builds s1disasm.

use std::fmt::{self, Write};

use anygenesis_shared::{Address, Size};

use crate::{
    Condition, Ea, IndexRegister, IndexSize, Instruction, Mnemonic, Operand, Register,
    RegisterList, instruction::branch_target,
};

/// The assembler syntax an instruction is printed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dialect {
    /// Musashi's conventions with lowercase registers, so the text can be compared with
    /// Musashi's: mnemonic padded to 8 columns, `, ` between operands, `(d16,An)`, raw
    /// PC-relative displacements, absolute branch targets without `.s`/`.w`, `dbra`.
    Motorola,
    /// The AS assembler as s1disasm uses it: a tab after the mnemonic, `,` between operands,
    /// uppercase hex, `sp` for A7, `d16(An)`, sign-extended `(xxx).w`, `.s`/`.w` on
    /// branches, `dbf`, and PC-relative operands as target addresses.
    As,
}

/// Names addresses for branch targets and PC-relative operands.
pub trait Labels {
    /// Returns the label for `address`, or `None` to print the address as a number.
    fn label(&self, address: Address) -> Option<String>;
}

/// Labels every address as `loc_XXXXXX` (six uppercase hex digits).
#[derive(Debug, Clone, Copy, Default)]
pub struct LocationLabels;

impl Labels for LocationLabels {
    fn label(&self, address: Address) -> Option<String> {
        Some(format!("loc_{:06X}", address.get()))
    }
}

/// How an instruction is printed.
#[derive(Clone, Copy)]
pub struct FormatOptions<'a> {
    /// The assembler syntax.
    pub dialect: Dialect,
    /// Labels for branch targets and PC-relative operands (listing mode); `None` prints
    /// every address as a number.
    pub labels: Option<&'a dyn Labels>,
}

impl<'a> FormatOptions<'a> {
    /// Options for `dialect` without labels.
    pub const fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            labels: None,
        }
    }

    /// Returns these options with `labels` naming branch and PC-relative targets.
    pub const fn with_labels(self, labels: &'a dyn Labels) -> Self {
        Self {
            labels: Some(labels),
            ..self
        }
    }
}

impl fmt::Debug for FormatOptions<'_> {
    // `dyn Labels` has no Debug, so only its presence is shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FormatOptions")
            .field("dialect", &self.dialect)
            .field("labels", &self.labels.is_some())
            .finish()
    }
}

impl Instruction {
    /// Returns a [`fmt::Display`] view of this instruction located at `address`.
    pub fn format<'a>(&'a self, address: Address, options: FormatOptions<'a>) -> Formatted<'a> {
        Formatted {
            instruction: self,
            address,
            options,
        }
    }
}

/// An instruction ready to print, from [`Instruction::format`].
#[derive(Debug, Clone, Copy)]
pub struct Formatted<'a> {
    instruction: &'a Instruction,
    address: Address,
    options: FormatOptions<'a>,
}

impl fmt::Display for Formatted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = String::new();
        if self.is_data() {
            self.write_data(&mut text)?;
        } else {
            self.write_instruction(&mut text)?;
        }
        f.write_str(&text)
    }
}

impl Formatted<'_> {
    fn is_motorola(&self) -> bool {
        self.options.dialect == Dialect::Motorola
    }

    fn mnemonic(&self) -> Mnemonic {
        self.instruction.mnemonic
    }

    /// Whether the instruction prints as `dc.w`: the line A and line F trap words, and in AS
    /// an empty MOVEM list, which has no assembler syntax.
    fn is_data(&self) -> bool {
        match self.mnemonic() {
            Mnemonic::LineA | Mnemonic::LineF => true,
            Mnemonic::Movem => {
                !self.is_motorola()
                    && self
                        .instruction
                        .operands
                        .contains(&Some(Operand::RegisterList(RegisterList(0))))
            }
            _ => false,
        }
    }

    fn write_data(&self, out: &mut String) -> fmt::Result {
        if self.is_motorola() {
            // Musashi prints the opcode word only, zero-padded.
            return write!(out, "dc.w    ${:04x}", self.instruction.opcode());
        }
        out.push_str("dc.w\t");
        for (index, word) in self.instruction.words().iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            write!(out, "${word:04X}")?;
        }
        Ok(())
    }

    fn write_instruction(&self, out: &mut String) -> fmt::Result {
        let mnemonic = self.mnemonic_text();
        let mut operands = self.instruction.operands.iter().flatten().peekable();
        if operands.peek().is_none() {
            out.push_str(&mnemonic);
            return Ok(());
        }
        let separator = if self.is_motorola() {
            write!(out, "{mnemonic:<7} ")?;
            ", "
        } else {
            write!(out, "{mnemonic}\t")?;
            ","
        };
        for (slot, operand) in operands.enumerate() {
            if slot > 0 {
                out.push_str(separator);
            }
            self.write_operand(out, slot, *operand)?;
        }
        Ok(())
    }

    fn mnemonic_text(&self) -> String {
        let mut text = match self.mnemonic() {
            Mnemonic::Bcc(condition) => format!("b{}", condition_name(condition)),
            Mnemonic::Dbcc(Condition::False) if self.is_motorola() => "dbra".to_owned(),
            Mnemonic::Dbcc(condition) => format!("db{}", condition_name(condition)),
            Mnemonic::Scc(condition) => format!("s{}", condition_name(condition)),
            mnemonic => base_name(mnemonic).to_owned(),
        };
        if let Some(suffix) = self.size_suffix() {
            text.push('.');
            text.push(suffix);
        }
        text
    }

    fn size_suffix(&self) -> Option<char> {
        let size = self.instruction.size?;
        let is_branch = matches!(
            self.mnemonic(),
            Mnemonic::Bcc(_) | Mnemonic::Bra | Mnemonic::Bsr
        );
        if is_branch {
            // The size is the displacement width, which only AS spells out.
            return (!self.is_motorola()).then_some(match size {
                Size::Byte => 's',
                _ => 'w',
            });
        }
        let unsized_form = if self.is_motorola() {
            self.is_unsized_in_musashi()
        } else {
            self.is_unsized_in_as()
        };
        (!unsized_form).then_some(size_char(size))
    }

    fn is_unsized_in_musashi(&self) -> bool {
        let special_register = self
            .instruction
            .operands
            .iter()
            .flatten()
            .any(|operand| matches!(operand, Operand::Ccr | Operand::Sr | Operand::Usp));
        special_register
            || matches!(
                self.mnemonic(),
                Mnemonic::Dbcc(_)
                    | Mnemonic::Scc(_)
                    | Mnemonic::Btst
                    | Mnemonic::Bchg
                    | Mnemonic::Bclr
                    | Mnemonic::Bset
                    | Mnemonic::Exg
                    | Mnemonic::Lea
                    | Mnemonic::Pea
                    | Mnemonic::Jmp
                    | Mnemonic::Jsr
                    | Mnemonic::Moveq
                    | Mnemonic::Swap
                    | Mnemonic::Nbcd
                    | Mnemonic::Tas
                    | Mnemonic::Abcd
                    | Mnemonic::Sbcd
                    | Mnemonic::Link
            )
    }

    /// The forms s1disasm's listing writes without a size (M0.4), DBcc included.
    fn is_unsized_in_as(&self) -> bool {
        matches!(
            self.mnemonic(),
            Mnemonic::Dbcc(_)
                | Mnemonic::Btst
                | Mnemonic::Bchg
                | Mnemonic::Bclr
                | Mnemonic::Bset
                | Mnemonic::Exg
                | Mnemonic::Lea
                | Mnemonic::Jmp
                | Mnemonic::Jsr
                | Mnemonic::Moveq
                | Mnemonic::Swap
        )
    }

    fn write_operand(&self, out: &mut String, slot: usize, operand: Operand) -> fmt::Result {
        match operand {
            Operand::Ea(ea) => self.write_ea(out, slot, ea),
            Operand::Quick(value) if self.mnemonic() == Mnemonic::Moveq => {
                out.push('#');
                self.write_signed(out, i32::from(value))
            }
            Operand::Quick(value) => write!(out, "#{value}"),
            Operand::Vector(vector) => {
                out.push('#');
                self.write_hex(out, u32::from(vector))
            }
            Operand::Branch(displacement) => {
                self.write_target(out, branch_target(self.address, displacement))
            }
            Operand::RegisterList(list) => self.write_register_list(out, list),
            Operand::Ccr => out.write_str("ccr"),
            Operand::Sr => out.write_str("sr"),
            Operand::Usp => out.write_str("usp"),
        }
    }

    fn write_ea(&self, out: &mut String, slot: usize, ea: Ea) -> fmt::Result {
        match ea {
            Ea::DataRegister(number) => self.write_register(out, Register::Data(number)),
            Ea::AddressRegister(number) => self.write_register(out, Register::Address(number)),
            Ea::Indirect(number) => self.write_indirect(out, "(", number, ")"),
            Ea::PostIncrement(number) => self.write_indirect(out, "(", number, ")+"),
            Ea::PreDecrement(number) => self.write_indirect(out, "-(", number, ")"),
            Ea::Displacement {
                register,
                displacement,
            } => {
                if self.is_motorola() {
                    out.push('(');
                    self.write_signed(out, i32::from(displacement))?;
                    self.write_indirect(out, ",", register, ")")
                } else {
                    self.write_signed(out, i32::from(displacement))?;
                    self.write_indirect(out, "(", register, ")")
                }
            }
            Ea::Index {
                register,
                displacement,
                index,
            } => {
                let base = Register::Address(register);
                self.write_indexed(out, i32::from(displacement), base, index)
            }
            Ea::AbsoluteShort(value) => {
                if self.is_motorola() {
                    write!(out, "${value:x}.w")
                } else {
                    // AS takes the address the CPU forms, so sign-extend it.
                    out.push('(');
                    self.write_hex(out, Size::Word.sign_extend(u32::from(value)) as u32)?;
                    out.push_str(").w");
                    Ok(())
                }
            }
            Ea::AbsoluteLong(value) => {
                if self.is_motorola() {
                    write!(out, "${value:x}.l")
                } else {
                    out.push('(');
                    self.write_hex(out, value)?;
                    out.push_str(").l");
                    Ok(())
                }
            }
            Ea::PcDisplacement(displacement) => {
                let target = self.extension_address(slot).offset(i32::from(displacement));
                self.write_pc_relative(out, i32::from(displacement), target, None)
            }
            Ea::PcIndex {
                displacement,
                index,
            } => {
                let target = self.extension_address(slot).offset(i32::from(displacement));
                self.write_pc_relative(out, i32::from(displacement), target, Some(index))
            }
            Ea::Immediate(value) => {
                out.push('#');
                self.write_immediate(out, value)
            }
        }
    }

    fn write_indirect(&self, out: &mut String, open: &str, number: u8, close: &str) -> fmt::Result {
        out.push_str(open);
        self.write_register(out, Register::Address(number))?;
        out.push_str(close);
        Ok(())
    }

    /// Writes `(d8,An,Xn)` (Motorola) or `d8(An,Xn)` (AS); a zero displacement is left out.
    fn write_indexed(
        &self,
        out: &mut String,
        displacement: i32,
        base: Register,
        index: IndexRegister,
    ) -> fmt::Result {
        if displacement == 0 {
            out.push('(');
        } else if self.is_motorola() {
            out.push('(');
            self.write_signed(out, displacement)?;
            out.push(',');
        } else {
            self.write_signed(out, displacement)?;
            out.push('(');
        }
        self.write_register(out, base)?;
        out.push(',');
        self.write_index(out, index)?;
        out.push(')');
        Ok(())
    }

    /// Writes a PC-relative operand. Motorola prints the raw displacement as Musashi does;
    /// AS prints the target address, from which the assembler computes the displacement. A
    /// label names the target in both.
    fn write_pc_relative(
        &self,
        out: &mut String,
        displacement: i32,
        target: Address,
        index: Option<IndexRegister>,
    ) -> fmt::Result {
        let label = self.label(target);
        if self.is_motorola() {
            out.push('(');
            match label {
                Some(label) => write!(out, "{label},")?,
                None if index.is_some() && displacement == 0 => {}
                None => {
                    self.write_signed(out, displacement)?;
                    out.push(',');
                }
            }
            out.push_str("pc");
        } else {
            match label {
                Some(label) => out.push_str(&label),
                None => self.write_hex(out, target.get())?,
            }
            out.push_str("(pc");
        }
        if let Some(index) = index {
            out.push(',');
            self.write_index(out, index)?;
        }
        out.push(')');
        Ok(())
    }

    fn extension_address(&self, slot: usize) -> Address {
        self.instruction.extension_address(self.address, slot)
    }

    /// Writes an immediate's value. Motorola follows Musashi's signedness per instruction;
    /// AS prints values unsigned except LINK's displacement.
    fn write_immediate(&self, out: &mut String, value: u32) -> fmt::Result {
        let signed_size = match self.mnemonic() {
            Mnemonic::Addi | Mnemonic::Subi | Mnemonic::Cmpi if self.is_motorola() => {
                self.instruction.size
            }
            Mnemonic::Stop if self.is_motorola() => Some(Size::Word),
            Mnemonic::Link => Some(Size::Word),
            _ => None,
        };
        match signed_size {
            Some(size) => self.write_signed(out, size.sign_extend(value)),
            None => self.write_hex(out, value),
        }
    }

    fn write_target(&self, out: &mut String, target: Address) -> fmt::Result {
        match self.label(target) {
            Some(label) => out.write_str(&label),
            None => self.write_hex(out, target.get()),
        }
    }

    fn label(&self, target: Address) -> Option<String> {
        self.options.labels.and_then(|labels| labels.label(target))
    }

    /// Writes a MOVEM set as Musashi does: data register runs, then address register runs,
    /// `/`-separated, a run of two or more as a range.
    fn write_register_list(&self, out: &mut String, list: RegisterList) -> fmt::Result {
        let is_set = |bit: u8| list.0 & (1 << bit) != 0;
        let mut first = true;
        let mut bit = 0;
        while bit < 16 {
            if !is_set(bit) {
                bit += 1;
                continue;
            }
            let start = bit;
            // A run stops at the end of its bank: D7 and A0 are never joined.
            while bit % 8 < 7 && is_set(bit + 1) {
                bit += 1;
            }
            if !first {
                out.push('/');
            }
            first = false;
            self.write_register(out, list_register(start))?;
            if bit > start {
                out.push('-');
                self.write_register(out, list_register(bit))?;
            }
            bit += 1;
        }
        Ok(())
    }

    fn write_index(&self, out: &mut String, index: IndexRegister) -> fmt::Result {
        self.write_register(out, index.register)?;
        out.push_str(match index.size {
            IndexSize::Word => ".w",
            IndexSize::Long => ".l",
        });
        Ok(())
    }

    fn write_register(&self, out: &mut String, register: Register) -> fmt::Result {
        match register {
            Register::Data(number) => write!(out, "d{number}"),
            Register::Address(7) if !self.is_motorola() => out.write_str("sp"),
            Register::Address(number) => write!(out, "a{number}"),
        }
    }

    /// Writes `-$` and the magnitude for negative values, else `$` and the value.
    fn write_signed(&self, out: &mut String, value: i32) -> fmt::Result {
        if value < 0 {
            out.push('-');
        }
        self.write_hex(out, value.unsigned_abs())
    }

    fn write_hex(&self, out: &mut String, value: u32) -> fmt::Result {
        if self.is_motorola() {
            write!(out, "${value:x}")
        } else {
            write!(out, "${value:X}")
        }
    }
}

/// The register a [`RegisterList`] bit stands for.
fn list_register(bit: u8) -> Register {
    if bit < 8 {
        Register::Data(bit)
    } else {
        Register::Address(bit - 8)
    }
}

fn size_char(size: Size) -> char {
    match size {
        Size::Byte => 'b',
        Size::Word => 'w',
        Size::Long => 'l',
    }
}

fn condition_name(condition: Condition) -> &'static str {
    match condition {
        Condition::True => "t",
        Condition::False => "f",
        Condition::Higher => "hi",
        Condition::LowerOrSame => "ls",
        Condition::CarryClear => "cc",
        Condition::CarrySet => "cs",
        Condition::NotEqual => "ne",
        Condition::Equal => "eq",
        Condition::OverflowClear => "vc",
        Condition::OverflowSet => "vs",
        Condition::Plus => "pl",
        Condition::Minus => "mi",
        Condition::GreaterOrEqual => "ge",
        Condition::Less => "lt",
        Condition::Greater => "gt",
        Condition::LessOrEqual => "le",
    }
}

/// The lowercase name of a mnemonic without condition; conditional ones are built by
/// `mnemonic_text`, and the trap words print as data.
fn base_name(mnemonic: Mnemonic) -> &'static str {
    match mnemonic {
        Mnemonic::Abcd => "abcd",
        Mnemonic::Add => "add",
        Mnemonic::Adda => "adda",
        Mnemonic::Addi => "addi",
        Mnemonic::Addq => "addq",
        Mnemonic::Addx => "addx",
        Mnemonic::And => "and",
        Mnemonic::Andi => "andi",
        Mnemonic::Asl => "asl",
        Mnemonic::Asr => "asr",
        Mnemonic::Bchg => "bchg",
        Mnemonic::Bclr => "bclr",
        Mnemonic::Bra => "bra",
        Mnemonic::Bset => "bset",
        Mnemonic::Bsr => "bsr",
        Mnemonic::Btst => "btst",
        Mnemonic::Chk => "chk",
        Mnemonic::Clr => "clr",
        Mnemonic::Cmp => "cmp",
        Mnemonic::Cmpa => "cmpa",
        Mnemonic::Cmpi => "cmpi",
        Mnemonic::Cmpm => "cmpm",
        Mnemonic::Divs => "divs",
        Mnemonic::Divu => "divu",
        Mnemonic::Eor => "eor",
        Mnemonic::Eori => "eori",
        Mnemonic::Exg => "exg",
        Mnemonic::Ext => "ext",
        Mnemonic::Illegal => "illegal",
        Mnemonic::Jmp => "jmp",
        Mnemonic::Jsr => "jsr",
        Mnemonic::Lea => "lea",
        Mnemonic::Link => "link",
        Mnemonic::Lsl => "lsl",
        Mnemonic::Lsr => "lsr",
        Mnemonic::Move => "move",
        Mnemonic::Movea => "movea",
        Mnemonic::Movem => "movem",
        Mnemonic::Movep => "movep",
        Mnemonic::Moveq => "moveq",
        Mnemonic::Muls => "muls",
        Mnemonic::Mulu => "mulu",
        Mnemonic::Nbcd => "nbcd",
        Mnemonic::Neg => "neg",
        Mnemonic::Negx => "negx",
        Mnemonic::Nop => "nop",
        Mnemonic::Not => "not",
        Mnemonic::Or => "or",
        Mnemonic::Ori => "ori",
        Mnemonic::Pea => "pea",
        Mnemonic::Reset => "reset",
        Mnemonic::Rol => "rol",
        Mnemonic::Ror => "ror",
        Mnemonic::Roxl => "roxl",
        Mnemonic::Roxr => "roxr",
        Mnemonic::Rte => "rte",
        Mnemonic::Rtr => "rtr",
        Mnemonic::Rts => "rts",
        Mnemonic::Sbcd => "sbcd",
        Mnemonic::Stop => "stop",
        Mnemonic::Sub => "sub",
        Mnemonic::Suba => "suba",
        Mnemonic::Subi => "subi",
        Mnemonic::Subq => "subq",
        Mnemonic::Subx => "subx",
        Mnemonic::Swap => "swap",
        Mnemonic::Tas => "tas",
        Mnemonic::Trap => "trap",
        Mnemonic::Trapv => "trapv",
        Mnemonic::Tst => "tst",
        Mnemonic::Unlk => "unlk",
        Mnemonic::Bcc(_) | Mnemonic::Dbcc(_) | Mnemonic::Scc(_) => "",
        Mnemonic::LineA | Mnemonic::LineF => "dc.w",
    }
}
