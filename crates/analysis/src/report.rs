use std::fmt;

use anygenesis_m68k::{Dialect, FormatOptions, Instruction};
use anygenesis_shared::Address;

use crate::{Analysis, Diagnostic, Source, Suggestion, UnresolvedSite};

/// Instructions shown before an unresolved site, so the reader can see how its register
/// or index was loaded.
const CONTEXT_INSTRUCTIONS: usize = 3;

/// A Markdown report of an [`Analysis`]: counts, every unresolved indirect site with a
/// what it most likely is, the diagnostics and the shared blocks.
#[derive(Debug, Clone, Copy)]
pub struct Report<'a> {
    analysis: &'a Analysis,
}

impl<'a> Report<'a> {
    pub(crate) fn new(analysis: &'a Analysis) -> Self {
        Self { analysis }
    }

    fn summary(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let analysis = self.analysis;
        let code_bytes: u32 = analysis
            .instructions
            .values()
            .map(Instruction::length)
            .sum();
        let tail_jumps: usize = analysis
            .functions
            .values()
            .map(|function| function.tail_jumps.len())
            .sum();
        let rows = [
            ("Functions", analysis.functions.len()),
            ("Basic blocks", analysis.blocks.len()),
            ("Shared blocks", analysis.shared_blocks().count()),
            ("Tail jumps", tail_jumps),
            ("Instructions", analysis.instructions.len()),
            ("Code bytes", code_bytes as usize),
            ("Jump tables used", analysis.jump_tables.len()),
            ("Unresolved indirect sites", analysis.unresolved.len()),
            ("Diagnostics", analysis.diagnostics.len()),
        ];
        writeln!(f, "## Summary\n")?;
        writeln!(f, "| Item | Count |")?;
        writeln!(f, "|------|------:|")?;
        for (item, count) in rows {
            writeln!(f, "| {item} | {count} |")?;
        }
        writeln!(f)
    }

    fn unresolved(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## Unresolved indirect jumps and calls\n")?;
        if self.analysis.unresolved.is_empty() {
            return writeln!(f, "None.\n");
        }
        for site in &self.analysis.unresolved {
            self.unresolved_site(f, site)?;
        }
        Ok(())
    }

    fn unresolved_site(&self, f: &mut fmt::Formatter<'_>, site: &UnresolvedSite) -> fmt::Result {
        let what = if site.is_call { "call" } else { "jump" };
        writeln!(f, "### {} ({what})\n", site.site)?;
        writeln!(f, "```text")?;
        for address in self.context(site.site) {
            if let Some(instruction) = self.analysis.instructions.get(&address) {
                let text = instruction.format(address, FormatOptions::new(Dialect::Motorola));
                writeln!(f, "{address}  {text}")?;
            }
        }
        writeln!(f, "```\n")?;
        match site.suggestion {
            Suggestion::JumpTable {
                address,
                kind,
                count_guess,
            } => {
                let count = match count_guess {
                    Some(count) => format!("{count} entries (guessed, verify)"),
                    None => "entries unknown (count by hand)".to_owned(),
                };
                writeln!(
                    f,
                    "Likely a jump table at {address}, {}, {count}.\n",
                    kind.name()
                )
            }
            Suggestion::Indirect => writeln!(f, "A jump through a register: targets unknown.\n"),
        }
    }

    /// Returns the site and up to [`CONTEXT_INSTRUCTIONS`] instructions running straight
    /// into it, in address order.
    fn context(&self, site: Address) -> Vec<Address> {
        let mut addresses = vec![site];
        let mut start = site;
        for (&address, instruction) in self.analysis.instructions.range(..site).rev() {
            if addresses.len() > CONTEXT_INSTRUCTIONS
                || address.wrapping_add(instruction.length()) != start
                || self.ends_flow(address)
            {
                break;
            }
            addresses.push(address);
            start = address;
        }
        addresses.reverse();
        addresses
    }

    /// Returns whether the instruction at `address` ends a block without falling through,
    /// so it cannot lead into the code after it.
    fn ends_flow(&self, address: Address) -> bool {
        self.analysis
            .blocks
            .range(..=address)
            .next_back()
            .is_some_and(|(_, block)| block.last == address && !block.terminator.falls_through())
    }

    fn diagnostics(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## Diagnostics\n")?;
        if self.analysis.diagnostics.is_empty() {
            return writeln!(f, "None.\n");
        }
        let mut sorted: Vec<&Diagnostic> = self.analysis.diagnostics.iter().collect();
        sorted.sort_by_key(|diagnostic| (diagnostic.order(), diagnostic.address()));
        let mut heading = None;
        for diagnostic in sorted {
            if heading != Some(diagnostic.order()) {
                if heading.is_some() {
                    writeln!(f)?;
                }
                heading = Some(diagnostic.order());
                writeln!(f, "### {}\n", diagnostic.title())?;
            }
            writeln!(f, "- {diagnostic}")?;
        }
        writeln!(f)
    }

    fn shared_blocks(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## Shared blocks\n")?;
        let mut shared = self.analysis.shared_blocks().peekable();
        if shared.peek().is_none() {
            return writeln!(f, "None.");
        }
        writeln!(f, "| Block | Functions |")?;
        writeln!(f, "|-------|-----------|")?;
        for block in shared {
            let functions: Vec<String> = block.functions.iter().map(Address::to_string).collect();
            writeln!(f, "| {} | {} |", block.start, functions.join(", "))?;
        }
        Ok(())
    }
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "# AnyGenesis analysis report\n")?;
        writeln!(f, "- ROM SHA-1: `{}`", self.analysis.sha1)?;
        writeln!(f, "- ROM size: {:#x} bytes\n", self.analysis.rom_size)?;
        self.summary(f)?;
        self.unresolved(f)?;
        self.diagnostics(f)?;
        self.shared_blocks(f)
    }
}

impl Diagnostic {
    /// Returns the report section order of this kind of diagnostic.
    fn order(&self) -> u8 {
        match self {
            Self::DecodeFailed { .. } => 0,
            Self::Overlap { .. } => 1,
            Self::OddTarget { .. } => 2,
            Self::OutOfRomTarget { .. } => 3,
            Self::RamTarget { .. } => 4,
            Self::DataTarget { .. } => 5,
            Self::FallsIntoData { .. } => 6,
            Self::RunsOffRom { .. } => 7,
            Self::NotBraW { .. } => 8,
            Self::TableOverlapsCode { .. } => 9,
            Self::TableSiteNotIndirect { .. } => 10,
            Self::UnownedBlock { .. } => 11,
            Self::SharedHandler { .. } => 12,
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::DecodeFailed { .. } => "Undecodable code",
            Self::Overlap { .. } => "Overlapping instructions",
            Self::OddTarget { .. } => "Odd targets",
            Self::OutOfRomTarget { .. } => "Targets outside the ROM",
            Self::RamTarget { .. } => "Targets in RAM",
            Self::DataTarget { .. } => "Targets in data regions",
            Self::FallsIntoData { .. } => "Code running into data regions",
            Self::RunsOffRom { .. } => "Code running off the ROM",
            Self::NotBraW { .. } => "bra_w entries that are not bra.w",
            Self::TableOverlapsCode { .. } => "Tables overlapping code",
            Self::TableSiteNotIndirect { .. } => "Table sites that are not indirect jumps",
            Self::UnownedBlock { .. } => "Blocks no function reaches",
            Self::SharedHandler { .. } => "Vectors sharing a handler",
        }
    }

    /// Returns the address the diagnostic is about, for sorting.
    fn address(&self) -> Address {
        match self {
            Self::Overlap { first: address, .. }
            | Self::OddTarget {
                target: address, ..
            }
            | Self::RamTarget {
                target: address, ..
            }
            | Self::OutOfRomTarget {
                target: address, ..
            }
            | Self::DataTarget {
                target: address, ..
            }
            | Self::DecodeFailed { address, .. }
            | Self::FallsIntoData { address, .. }
            | Self::RunsOffRom { address }
            | Self::SharedHandler {
                target: address, ..
            }
            | Self::NotBraW { entry: address, .. }
            | Self::TableOverlapsCode { table: address, .. }
            | Self::TableSiteNotIndirect { table: address, .. }
            | Self::UnownedBlock { start: address } => *address,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlap { first, second } => {
                write!(f, "{second} starts inside the instruction at {first}")
            }
            Self::OddTarget { source, target }
            | Self::RamTarget { source, target }
            | Self::OutOfRomTarget { source, target } => write!(f, "{target} from {source}"),
            Self::DataTarget {
                source,
                target,
                region,
            } => write!(f, "{target} from {source}, in the data region at {region}"),
            Self::DecodeFailed { address, error } => write!(f, "{address}: {error}"),
            Self::FallsIntoData { address, region } => {
                write!(f, "after {address}, into the data region at {region}")
            }
            Self::RunsOffRom { address } => write!(f, "after {address}"),
            Self::SharedHandler { target, vectors } => {
                let list: Vec<String> = vectors.iter().map(u8::to_string).collect();
                write!(f, "{target} handles vectors {}", list.join(", "))
            }
            Self::NotBraW { table, entry } => write!(f, "{entry} in the table at {table}"),
            Self::TableOverlapsCode { table, instruction } => {
                write!(
                    f,
                    "the table at {table} overlaps the instruction at {instruction}"
                )
            }
            Self::TableSiteNotIndirect { table, site } => {
                write!(f, "the table at {table} names site {site}")
            }
            Self::UnownedBlock { start } => write!(f, "{start}"),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instruction(address) => write!(f, "the instruction at {address}"),
            Self::Vector(index) => write!(f, "vector {index}"),
            Self::Hint => write!(f, "a hint"),
        }
    }
}
