use std::collections::{BTreeMap, BTreeSet};

use anygenesis_m68k::{
    Ea, IndexRegister, Instruction, MAX_WORDS, Mnemonic, Operand, Register, decode_at,
};
use anygenesis_rom::vector_name;
use anygenesis_shared::{Address, Size, read_u16_be};

use crate::{
    Call, CallKind, Diagnostic, Edge, EdgeKind, EntryKind, Hints, JumpTable, Origin, ResolvedTable,
    Source, Suggestion, TargetKind, Terminator, UnresolvedSite, tables,
};

/// Genesis work RAM and its mirrors, 0xE00000-0xFFFFFF.
const RAM_START: u32 = 0xE0_0000;

/// The first and one past the last vector that can hold a handler: reset PC through
/// TRAP #15.
const FIRST_HANDLER_VECTOR: usize = 2;
const END_HANDLER_VECTORS: usize = 48;

/// TRAP #n reads vector 32 + n.
const FIRST_TRAP_VECTOR: u32 = 32;

/// What an instruction does to control flow, for the instructions that end a block or
/// make a call.
#[derive(Debug, Default)]
pub(crate) struct Effect {
    pub terminator: Option<Terminator>,
    /// Edges other than the fallthrough, which the block builder adds.
    pub edges: Vec<Edge>,
    pub call: Option<Call>,
}

/// A function entry found during the descent.
#[derive(Debug, Default)]
pub(crate) struct Entry {
    pub origins: Vec<Origin>,
    pub callers: BTreeSet<Address>,
}

/// Recursive descent over the ROM: decodes every reachable instruction and records the
/// control flow of each, without forming blocks yet.
pub(crate) struct Discovery<'a> {
    bytes: &'a [u8],
    vectors: &'a [u32; 64],
    hints: &'a Hints,
    pub instructions: BTreeMap<Address, Instruction>,
    pub effects: BTreeMap<Address, Effect>,
    /// Addresses where a block must start.
    pub leaders: BTreeSet<Address>,
    pub entries: BTreeMap<Address, Entry>,
    pub tables: Vec<ResolvedTable>,
    pub unresolved: Vec<UnresolvedSite>,
    pub diagnostics: Vec<Diagnostic>,
    worklist: Vec<Address>,
}

impl<'a> Discovery<'a> {
    pub fn new(bytes: &'a [u8], vectors: &'a [u32; 64], hints: &'a Hints) -> Self {
        Self {
            bytes,
            vectors,
            hints,
            instructions: BTreeMap::new(),
            effects: BTreeMap::new(),
            leaders: BTreeSet::new(),
            entries: BTreeMap::new(),
            tables: Vec::new(),
            unresolved: Vec::new(),
            diagnostics: Vec::new(),
            worklist: Vec::new(),
        }
    }

    /// Descends from every root until nothing new is reached.
    pub fn run(&mut self) {
        let hints = self.hints;
        self.add_vector_roots();
        for hint in &hints.functions {
            self.add_entry(hint.address, Some(Origin::Hint), None, Source::Hint);
        }
        self.drain();
        // Tables no reached site claimed are roots of their own; their code can reach sites
        // that claim further tables, so repeat until none is left.
        loop {
            let unclaimed: Vec<&JumpTable> = hints
                .jump_tables
                .iter()
                .filter(|table| !self.tables.iter().any(|used| used.address == table.address))
                .collect();
            if unclaimed.is_empty() {
                break;
            }
            for table in unclaimed {
                if let Some(site) = table.site
                    && self.instructions.contains_key(&site)
                {
                    self.diagnose(Diagnostic::TableSiteNotIndirect {
                        table: table.address,
                        site,
                    });
                }
                self.resolve_table(table, None, false);
            }
            self.drain();
        }
        self.find_overlaps();
        self.find_tables_over_code();
    }

    fn add_vector_roots(&mut self) {
        let mut handlers: BTreeMap<Address, Vec<u8>> = BTreeMap::new();
        let reset = Address::new(self.vectors[1]);
        self.add_entry(reset, Some(Origin::Reset), None, Source::Vector(1));
        for index in FIRST_HANDLER_VECTOR..END_HANDLER_VECTORS {
            // Reserved vectors are not handlers, whatever they hold.
            if vector_name(index) == Some("reserved") {
                continue;
            }
            // The index is below 48, so it fits a byte.
            let vector = index as u8;
            let target = Address::new(self.vectors[index]);
            if self.add_entry(
                target,
                Some(Origin::Vector(vector)),
                None,
                Source::Vector(vector),
            ) {
                handlers.entry(target).or_default().push(vector);
            }
        }
        for (target, vectors) in handlers {
            if vectors.len() > 1 {
                self.diagnose(Diagnostic::SharedHandler { target, vectors });
            }
        }
    }

    fn drain(&mut self) {
        while let Some(start) = self.worklist.pop() {
            self.decode_run(start);
        }
    }

    /// Decodes straight-line code from `start` until control leaves it or it joins code
    /// already decoded.
    fn decode_run(&mut self, start: Address) {
        let mut address = start;
        let mut previous = None;
        loop {
            if self.instructions.contains_key(&address) {
                if previous.is_some() {
                    // Joining code decoded earlier: it must start a block, or a block of
                    // this run and one of the earlier run would share its instructions.
                    self.leaders.insert(address);
                }
                return;
            }
            let instruction = match decode_at(self.bytes, address.get() as usize) {
                Ok(instruction) => instruction,
                Err(error) => {
                    self.diagnose(Diagnostic::DecodeFailed { address, error });
                    self.end_invalid(previous);
                    return;
                }
            };
            self.instructions.insert(address, instruction);
            let next = address.wrapping_add(instruction.length());
            if !self.follow(address, &instruction) {
                return;
            }
            if next.get() >= self.rom_end() {
                self.diagnose(Diagnostic::RunsOffRom { address });
                self.end_invalid(Some(address));
                return;
            }
            if let Some(region) = self.hints.data_region(next) {
                self.diagnose(Diagnostic::FallsIntoData {
                    address,
                    region: region.start,
                });
                self.end_invalid(Some(address));
                return;
            }
            if self
                .effects
                .get(&address)
                .and_then(|effect| effect.terminator)
                .is_some()
            {
                self.leaders.insert(next);
            }
            previous = Some(address);
            address = next;
        }
    }

    /// Marks the block ending at `last` as running into bytes that are not code, unless its
    /// last instruction already ends it.
    fn end_invalid(&mut self, last: Option<Address>) {
        if let Some(last) = last {
            self.effects
                .entry(last)
                .or_default()
                .terminator
                .get_or_insert(Terminator::Invalid);
        }
    }

    /// Records the control flow of one instruction; returns whether execution continues
    /// after it.
    fn follow(&mut self, site: Address, instruction: &Instruction) -> bool {
        let source = Source::Instruction(site);
        match instruction.mnemonic {
            Mnemonic::Bcc(_) | Mnemonic::Dbcc(_) | Mnemonic::Bra => {
                let Some(target) = instruction.branch_target(site) else {
                    return true;
                };
                let (terminator, kind) = match instruction.mnemonic {
                    Mnemonic::Bcc(_) => (Terminator::Conditional, EdgeKind::Taken),
                    Mnemonic::Dbcc(_) => (Terminator::DecrementBranch, EdgeKind::Taken),
                    _ => (Terminator::Jump, EdgeKind::Jump),
                };
                self.add_jump(source, target);
                self.end(site, terminator, vec![Edge { target, kind }]);
                terminator.falls_through()
            }
            Mnemonic::Bsr => match instruction.branch_target(site) {
                Some(target) => self.call(site, CallKind::Bsr, target),
                None => true,
            },
            Mnemonic::Jsr | Mnemonic::Jmp => {
                let is_call = instruction.mnemonic == Mnemonic::Jsr;
                let Some(Operand::Ea(ea)) = instruction.operands[0] else {
                    return true;
                };
                match (direct_target(site, ea), is_call) {
                    (Some(target), true) => self.call(site, CallKind::Jsr, target),
                    (Some(target), false) => {
                        self.add_jump(source, target);
                        let edge = Edge {
                            target,
                            kind: EdgeKind::Jump,
                        };
                        self.end(site, Terminator::Jump, vec![edge]);
                        false
                    }
                    (None, _) => self.indirect(site, ea, is_call),
                }
            }
            Mnemonic::Rts => self.end(site, Terminator::Return, Vec::new()),
            Mnemonic::Rte => self.end(site, Terminator::ReturnFromException, Vec::new()),
            Mnemonic::Rtr => self.end(site, Terminator::ReturnRestoringCcr, Vec::new()),
            Mnemonic::Stop => self.end(site, Terminator::Stop, Vec::new()),
            Mnemonic::Illegal | Mnemonic::LineA | Mnemonic::LineF => {
                self.end(site, Terminator::Exception, Vec::new())
            }
            Mnemonic::Trap => {
                let Some(Operand::Vector(number)) = instruction.operands[0] else {
                    return true;
                };
                let index = (FIRST_TRAP_VECTOR + u32::from(number)) as usize;
                let handler = Address::new(self.vectors[index]);
                self.effects.entry(site).or_default().call = Some(Call {
                    site,
                    kind: CallKind::Trap,
                    targets: vec![handler],
                });
                // The handler is already a root through its vector.
                self.end(site, Terminator::Trap, Vec::new())
            }
            _ => true,
        }
    }

    /// Ends a block at `site`; returns whether execution falls through.
    fn end(&mut self, site: Address, terminator: Terminator, edges: Vec<Edge>) -> bool {
        let effect = self.effects.entry(site).or_default();
        effect.terminator = Some(terminator);
        effect.edges = edges;
        terminator.falls_through()
    }

    /// Records a direct call; returns whether execution continues after it.
    fn call(&mut self, site: Address, kind: CallKind, target: Address) -> bool {
        self.add_entry(target, None, Some(site), Source::Instruction(site));
        self.effects.entry(site).or_default().call = Some(Call {
            site,
            kind,
            targets: vec![target],
        });
        let noreturn = self
            .hints
            .function(target)
            .is_some_and(|hint| hint.noreturn);
        if noreturn {
            self.end(site, Terminator::NoReturnCall, Vec::new());
        }
        !noreturn
    }

    /// Resolves a JMP or JSR through a register or table; returns whether execution
    /// continues after it.
    fn indirect(&mut self, site: Address, ea: Ea, is_call: bool) -> bool {
        let pc_table = match ea {
            // The extension word follows the opcode.
            Ea::PcIndex {
                displacement,
                index,
            } => Some((site.wrapping_add(2).offset(i32::from(displacement)), index)),
            _ => None,
        };
        let hints = self.hints;
        let targets = if let Some(hint) = hints.indirect(site) {
            for &target in &hint.targets {
                if is_call {
                    let origin = Origin::Indirect { site };
                    self.add_entry(target, Some(origin), Some(site), Source::Hint);
                } else {
                    self.add_jump(Source::Hint, target);
                }
            }
            Some((hint.targets.clone(), EdgeKind::Hinted))
        } else if let Some(table) = hints
            .jump_tables
            .iter()
            .find(|table| table.site == Some(site))
            .or_else(|| pc_table.and_then(|(address, _)| hints.jump_table(address)))
        {
            let targets = self.resolve_table(table, Some(site), is_call);
            Some((
                targets,
                EdgeKind::Table {
                    table: table.address,
                },
            ))
        } else {
            let suggestion = match pc_table {
                Some((address, index)) => {
                    let kind = if self.word_table_shape(site, index, address) {
                        EntryKind::WordOffset
                    } else {
                        EntryKind::BraW
                    };
                    Suggestion::JumpTable {
                        address,
                        kind,
                        count_guess: tables::guess_count(self.bytes, address, kind),
                    }
                }
                None => Suggestion::Indirect,
            };
            self.unresolved.push(UnresolvedSite {
                site,
                is_call,
                suggestion,
            });
            None
        };
        let (targets, kind) = targets.unwrap_or((Vec::new(), EdgeKind::Hinted));
        if is_call {
            self.effects.entry(site).or_default().call = Some(Call {
                site,
                kind: CallKind::Jsr,
                targets,
            });
            true
        } else {
            let edges = targets
                .into_iter()
                .map(|target| Edge { target, kind })
                .collect();
            self.end(site, Terminator::IndirectJump, edges)
        }
    }

    /// Returns whether the instruction before `site` is `move.w Tbl(pc,dx.w),dn`, reading
    /// the same table into the register the jump at `site` indexes with: the shape of a
    /// table of word offsets.
    fn word_table_shape(&self, site: Address, index: IndexRegister, table: Address) -> bool {
        let Some((&start, previous)) = self.instructions.range(..site).next_back() else {
            return false;
        };
        if start.wrapping_add(previous.length()) != site
            || previous.mnemonic != Mnemonic::Move
            || previous.size != Some(Size::Word)
        {
            return false;
        }
        match previous.operands {
            [
                Some(Operand::Ea(Ea::PcIndex { displacement, .. })),
                Some(Operand::Ea(Ea::DataRegister(register))),
            ] => {
                index.register == Register::Data(register)
                    && start.wrapping_add(2).offset(i32::from(displacement)) == table
            }
            _ => false,
        }
    }

    /// Decodes a table's entries and queues their targets; returns the targets.
    fn resolve_table(
        &mut self,
        table: &JumpTable,
        site: Option<Address>,
        is_call: bool,
    ) -> Vec<Address> {
        // Calls always lead to functions; a site's jump defaults to blocks of its function,
        // a table without a site to separate functions.
        let target_kind = match (is_call, site, table.target) {
            (true, _, _) => TargetKind::Function,
            (false, _, Some(kind)) => kind,
            (false, Some(_), None) => TargetKind::Block,
            (false, None, None) => TargetKind::Function,
        };
        let source = Source::Hint;
        let mut targets = Vec::new();
        for index in 0..table.count {
            let Some(target) = tables::entry_target(self.bytes, table.address, table.kind, index)
            else {
                let entry = table.address.wrapping_add(index * table.kind.entry_bytes());
                self.diagnose(Diagnostic::OutOfRomTarget {
                    source,
                    target: entry,
                });
                break;
            };
            if table.kind == EntryKind::BraW && self.word_at(target) != Some(tables::BRA_W) {
                self.diagnose(Diagnostic::NotBraW {
                    table: table.address,
                    entry: target,
                });
            }
            if target_kind == TargetKind::Function {
                let origin = Origin::Table {
                    table: table.address,
                };
                self.add_entry(target, Some(origin), site.filter(|_| is_call), source);
            } else {
                self.add_jump(source, target);
            }
            targets.push(target);
        }
        self.tables.push(ResolvedTable {
            address: table.address,
            kind: table.kind,
            count: table.count,
            site,
            target: target_kind,
            targets: targets.clone(),
        });
        targets
    }

    /// Records a function entry and queues it; returns whether the target is code to
    /// decode.
    fn add_entry(
        &mut self,
        target: Address,
        origin: Option<Origin>,
        caller: Option<Address>,
        source: Source,
    ) -> bool {
        if !self.check_target(source, target) {
            return false;
        }
        let entry = self.entries.entry(target).or_default();
        if let Some(origin) = origin
            && !entry.origins.contains(&origin)
        {
            entry.origins.push(origin);
        }
        entry.callers.extend(caller);
        self.leaders.insert(target);
        self.worklist.push(target);
        true
    }

    /// Queues a jump target as the start of a block.
    fn add_jump(&mut self, source: Source, target: Address) {
        if self.check_target(source, target) {
            self.leaders.insert(target);
            self.worklist.push(target);
        }
    }

    /// Returns whether `target` can be decoded, diagnosing why not.
    fn check_target(&mut self, source: Source, target: Address) -> bool {
        let diagnostic = if !target.is_even() {
            Diagnostic::OddTarget { source, target }
        } else if target.get() >= RAM_START {
            Diagnostic::RamTarget { source, target }
        } else if target.get() >= self.rom_end() {
            Diagnostic::OutOfRomTarget { source, target }
        } else if let Some(region) = self.hints.data_region(target) {
            Diagnostic::DataTarget {
                source,
                target,
                region: region.start,
            }
        } else {
            return true;
        };
        self.diagnose(diagnostic);
        false
    }

    /// Flags every instruction that starts inside the one before it.
    fn find_overlaps(&mut self) {
        let overlaps: Vec<Diagnostic> = self
            .instructions
            .iter()
            .zip(self.instructions.keys().skip(1))
            .filter_map(|((&first, instruction), &second)| {
                (first.wrapping_add(instruction.length()) > second)
                    .then_some(Diagnostic::Overlap { first, second })
            })
            .collect();
        overlaps
            .into_iter()
            .for_each(|diagnostic| self.diagnose(diagnostic));
    }

    /// Flags decoded instructions that share bytes with a table of offsets or pointers.
    fn find_tables_over_code(&mut self) {
        let mut found = Vec::new();
        for table in &self.tables {
            if table.kind == EntryKind::BraW {
                continue;
            }
            let end = table
                .address
                .wrapping_add(table.count * table.kind.entry_bytes());
            // The longest instruction that can reach into the table starts this far before it.
            let reach = MAX_WORDS as u32 * 2 - 2;
            let earliest = Address::new(table.address.get().saturating_sub(reach));
            for (&instruction, decoded) in self.instructions.range(earliest..end) {
                if instruction.wrapping_add(decoded.length()) > table.address {
                    found.push(Diagnostic::TableOverlapsCode {
                        table: table.address,
                        instruction,
                    });
                }
            }
        }
        found
            .into_iter()
            .for_each(|diagnostic| self.diagnose(diagnostic));
    }

    fn word_at(&self, address: Address) -> Option<u16> {
        read_u16_be(self.bytes, address.get() as usize).ok()
    }

    fn rom_end(&self) -> u32 {
        // The ROM loader caps images at 4 MiB.
        self.bytes.len() as u32
    }

    fn diagnose(&mut self, diagnostic: Diagnostic) {
        if !self.diagnostics.contains(&diagnostic) {
            self.diagnostics.push(diagnostic);
        }
    }
}

/// Returns the fixed target of a JMP or JSR, or `None` when it goes through a register.
fn direct_target(site: Address, ea: Ea) -> Option<Address> {
    match ea {
        // The CPU sign-extends a short absolute address.
        Ea::AbsoluteShort(raw) => Some(Address::new(raw as i16 as u32)),
        Ea::AbsoluteLong(raw) => Some(Address::new(raw)),
        // The extension word follows the opcode.
        Ea::PcDisplacement(displacement) => {
            Some(site.wrapping_add(2).offset(i32::from(displacement)))
        }
        _ => None,
    }
}
