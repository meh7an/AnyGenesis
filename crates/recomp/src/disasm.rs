//! Whole-ROM listing in the AS dialect: discovered code as instructions, everything else as
//! `dc.w`/`dc.b`, with each line's address and bytes in a comment. The text assembles back
//! with the AS assembler that builds s1disasm.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::ops::Bound;

use anygenesis_analysis::{Analysis, EntryKind, ResolvedTable};
use anygenesis_m68k::{Dialect, Ea, FormatOptions, Instruction, Labels, Operand};
use anygenesis_shared::Address;

/// The comment with address and bytes starts at this column (tabs stop every 8).
const COMMENT_COLUMN: usize = 48;

/// Data words per `dc.w` line.
const WORDS_PER_LINE: u32 = 8;

/// Writes the listing of `rom` as `analysis` describes it, and returns the 1-based line of
/// every instruction printed as such, for `#line` directives in generated C.
///
/// # Errors
/// Returns the first error `out` reports.
pub fn write_listing(
    out: &mut dyn Write,
    rom: &[u8],
    analysis: &Analysis,
) -> io::Result<BTreeMap<Address, u32>> {
    let listing = Listing::new(rom, analysis);
    let mut counter = LineCounter {
        out,
        lines: 0,
        instructions: BTreeMap::new(),
    };
    listing.write(&mut counter)?;
    Ok(counter.instructions)
}

/// Counts the lines written through it and records where instructions went.
struct LineCounter<'w> {
    out: &'w mut dyn Write,
    lines: u32,
    instructions: BTreeMap<Address, u32>,
}

impl Write for LineCounter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.out.write(buf)?;
        let newlines = buf[..written].iter().filter(|&&byte| byte == b'\n').count();
        // A listing has far fewer than 2^32 lines.
        self.lines += newlines as u32;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// What one listing line kind is named after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LabelKind {
    Function,
    Code,
    Table,
    Data,
}

impl LabelKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Code => "loc",
            Self::Table => "tbl",
            Self::Data => "dat",
        }
    }
}

/// How far ahead a target without a line of its own looks for a label to count back from.
const NEXT_LABEL_REACH: u32 = 0x80;

/// Labels of the addresses the listing gives a line of their own.
struct LabelMap(BTreeMap<Address, String>);

impl Labels for LabelMap {
    /// Names a target without a line of its own relative to a nearby label, as `Tbl-4`: AS
    /// refuses a bare target number in a PC-indexed operand with a range overflow.
    fn label(&self, address: Address) -> Option<String> {
        if let Some(label) = self.0.get(&address) {
            return Some(label.clone());
        }
        if let Some((next, label)) = self.0.range(address..).next()
            && next.get() - address.get() <= NEXT_LABEL_REACH
        {
            return Some(format!("{label}-{}", next.get() - address.get()));
        }
        self.0
            .range(..address)
            .next_back()
            .map(|(previous, label)| format!("{label}+{}", address.get() - previous.get()))
    }
}

/// Collects the branch and PC-relative targets the formatter asks labels for.
struct Recorder(RefCell<Vec<Address>>);

impl Labels for Recorder {
    fn label(&self, address: Address) -> Option<String> {
        self.0.borrow_mut().push(address);
        None
    }
}

struct Listing<'a> {
    rom: &'a [u8],
    sha1: &'a str,
    /// The instructions printed as such: overlapped ones are left inside their predecessor.
    code: BTreeMap<Address, &'a Instruction>,
    /// Hinted tables printed entry by entry, by start address.
    tables: BTreeMap<Address, &'a ResolvedTable>,
    labels: LabelMap,
}

impl<'a> Listing<'a> {
    fn new(rom: &'a [u8], analysis: &'a Analysis) -> Self {
        let mut code = BTreeMap::new();
        let mut next = Address::new(0);
        for (&address, instruction) in &analysis.instructions {
            if address >= next {
                code.insert(address, instruction);
                next = address.wrapping_add(instruction.length());
            }
        }
        let rom_end = Address::new(rom.len() as u32);
        let is_free = |address: Address| {
            address < rom_end
                && code
                    .range(..=address)
                    .next_back()
                    .is_none_or(|(&start, instruction)| {
                        start == address || address >= start.wrapping_add(instruction.length())
                    })
        };

        // Tables overlapping printed code stay plain data.
        let tables = analysis
            .jump_tables
            .iter()
            .filter(|table| matches!(table.kind, EntryKind::WordOffset | EntryKind::Long))
            .filter(|table| {
                let end = table
                    .address
                    .wrapping_add(table.count * table.kind.entry_bytes());
                table.address < end
                    && end <= rom_end
                    && is_free(table.address)
                    && code.range(table.address..end).next().is_none()
            })
            .map(|table| (table.address, table))
            .collect();

        let mut candidates: BTreeMap<Address, LabelKind> = BTreeMap::new();
        let mut propose = |address: Address, kind: LabelKind| {
            let entry = candidates.entry(address).or_insert(kind);
            *entry = (*entry).min(kind);
        };
        for &entry in analysis.functions.keys() {
            propose(entry, LabelKind::Function);
        }
        for &start in analysis.blocks.keys() {
            propose(start, LabelKind::Code);
        }
        for table in &analysis.jump_tables {
            propose(table.address, LabelKind::Table);
        }
        let recorder = Recorder(RefCell::new(Vec::new()));
        for (&address, instruction) in &code {
            let options = FormatOptions::new(Dialect::As).with_labels(&recorder);
            let _ = instruction.format(address, options).to_string();
        }
        for target in recorder.0.into_inner() {
            let kind = if code.contains_key(&target) {
                LabelKind::Code
            } else {
                LabelKind::Data
            };
            propose(target, kind);
        }

        let labels = candidates
            .into_iter()
            .filter(|&(address, _)| is_free(address))
            .map(|(address, kind)| {
                let kind = match kind {
                    LabelKind::Function | LabelKind::Code if !code.contains_key(&address) => {
                        LabelKind::Data
                    }
                    other => other,
                };
                (address, format!("{}_{:06X}", kind.prefix(), address.get()))
            })
            .collect();

        Self {
            rom,
            sha1: &analysis.sha1,
            code,
            tables,
            labels: LabelMap(labels),
        }
    }

    fn write(&self, out: &mut LineCounter<'_>) -> io::Result<()> {
        writeln!(out, "; ROM SHA-1 {}", self.sha1)?;
        writeln!(out, "\tcpu\t68000")?;
        writeln!(out, "\tsupmode\ton")?;
        writeln!(out, "\tpadding\toff")?;
        let end = Address::new(self.rom.len() as u32);
        let mut position = Address::new(0);
        while position < end {
            if let Some(label) = self.labels.0.get(&position) {
                writeln!(out, "{label}:")?;
            }
            position = if let Some(instruction) = self.code.get(&position) {
                out.instructions.insert(position, out.lines + 1);
                self.write_instruction(out, position, instruction)?
            } else if let Some(table) = self.tables.get(&position) {
                self.write_table(out, table)?
            } else {
                self.write_data(out, position, end)?
            };
        }
        writeln!(out, "\tend")
    }

    fn write_instruction(
        &self,
        out: &mut dyn Write,
        address: Address,
        instruction: &Instruction,
    ) -> io::Result<Address> {
        let options = FormatOptions::new(Dialect::As).with_labels(&self.labels);
        let formatted = instruction.format(address, options).to_string();
        let words: Vec<String> = instruction
            .words()
            .iter()
            .map(|word| format!("{word:04X}"))
            .collect();
        if has_zero_displacement(instruction) {
            // AS assembles `0(An)` as `(An)`, two bytes shorter, so the words go out as data.
            let data: Vec<String> = words.iter().map(|word| format!("${word}")).collect();
            let text = format!("\tdc.w\t{}", data.join(","));
            let comment = format!("{}  {}", words.join(" "), formatted.replace('\t', " "));
            write_line(out, &text, address, &comment)?;
        } else {
            write_line(out, &format!("\t{formatted}"), address, &words.join(" "))?;
        }
        Ok(address.wrapping_add(instruction.length()))
    }

    fn write_table(&self, out: &mut dyn Write, table: &ResolvedTable) -> io::Result<Address> {
        let step = table.kind.entry_bytes();
        let base = self.labels.0.get(&table.address);
        for (index, &target) in table.targets.iter().enumerate() {
            let address = table.address.wrapping_add(index as u32 * step);
            let bytes = self.bytes(address, step);
            let raw = bytes
                .iter()
                .fold(0u32, |value, &byte| value << 8 | u32::from(byte));
            let target_label = self.labels.0.get(&target);
            // Symbolic entries only where they assemble back to the stored value.
            let text = match table.kind {
                EntryKind::WordOffset => match (target_label, base) {
                    (Some(target_label), Some(base))
                        if target.wrapping_sub(table.address.get()).get() as u16 == raw as u16 =>
                    {
                        format!("\tdc.w\t{target_label}-{base}")
                    }
                    _ => format!("\tdc.w\t${raw:04X}"),
                },
                _ => match target_label {
                    Some(target_label) if raw == target.get() => {
                        format!("\tdc.l\t{target_label}")
                    }
                    _ => format!("\tdc.l\t${raw:08X}"),
                },
            };
            write_line(out, &text, address, &hex_bytes(bytes))?;
        }
        Ok(table.address.wrapping_add(table.count * step))
    }

    /// Writes data up to the next instruction, table or label.
    fn write_data(&self, out: &mut dyn Write, start: Address, end: Address) -> io::Result<Address> {
        let after = (Bound::Excluded(start), Bound::Unbounded);
        let stop = [
            self.code.range(after).next().map(|(&next, _)| next),
            self.tables.range(after).next().map(|(&next, _)| next),
            self.labels.0.range(after).next().map(|(&next, _)| next),
        ]
        .into_iter()
        .flatten()
        .fold(end, Address::min);

        let mut position = start;
        if !position.is_even() {
            position = self.write_byte(out, position)?;
        }
        while stop.get() - position.get() >= 2 {
            let count = ((stop.get() - position.get()) / 2).min(WORDS_PER_LINE);
            let words: Vec<String> = self
                .bytes(position, count * 2)
                .chunks(2)
                .map(|pair| format!("${:02X}{:02X}", pair[0], pair[1]))
                .collect();
            write_line(out, &format!("\tdc.w\t{}", words.join(",")), position, "")?;
            position = position.wrapping_add(count * 2);
        }
        if position < stop {
            position = self.write_byte(out, position)?;
        }
        Ok(position)
    }

    fn write_byte(&self, out: &mut dyn Write, address: Address) -> io::Result<Address> {
        let byte = self.bytes(address, 1)[0];
        write_line(out, &format!("\tdc.b\t${byte:02X}"), address, "")?;
        Ok(address.wrapping_add(1))
    }

    fn bytes(&self, address: Address, length: u32) -> &[u8] {
        let start = address.get() as usize;
        &self.rom[start..start + length as usize]
    }
}

fn has_zero_displacement(instruction: &Instruction) -> bool {
    instruction.operands.iter().flatten().any(|operand| {
        matches!(
            operand,
            Operand::Ea(Ea::Displacement {
                displacement: 0,
                ..
            })
        )
    })
}

fn write_line(out: &mut dyn Write, text: &str, address: Address, bytes: &str) -> io::Result<()> {
    let width = text.chars().fold(0, |column, c| {
        if c == '\t' {
            (column / 8 + 1) * 8
        } else {
            column + 1
        }
    });
    let padding = COMMENT_COLUMN.saturating_sub(width).max(1);
    let comment = if bytes.is_empty() {
        format!("; {:06X}", address.get())
    } else {
        format!("; {:06X}: {bytes}", address.get())
    };
    writeln!(out, "{text}{:padding$}{comment}", "")
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join("")
}
