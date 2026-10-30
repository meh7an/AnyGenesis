//! Finds the jump tables that indirect jumps and calls read, from the code before them, so
//! discovery can follow them without hints.
//!
//! A table's start and kind come from the dispatch idiom; its size from a bound on the index
//! (`andi` or `cmp` + Bcc) when there is one, and otherwise from where its entries stop
//! looking like code. Every entry past the real end that still passes becomes an optimised
//! function nobody calls, which costs size but never correctness: every address has stepped
//! code anyway.

use std::collections::{BTreeMap, BTreeSet};

use anygenesis_m68k::{Condition, Ea, Instruction, Mnemonic, Operand, Register, decode_at};
use anygenesis_shared::{Address, Size};

use crate::{
    EntryKind, FunctionHint, Hints, IndirectHint, JumpTable,
    discover::Discovery,
    tables::{self, BRA_W, MAX_ENTRIES},
};

/// Discovery rounds before the fixpoint gives up; both Sonic games settle in under 8.
const MAX_ROUNDS: usize = 64;
/// Straight-line instructions looked at before a dispatch.
const LOOKBACK: usize = 12;
/// Instructions a table entry's target must decode cleanly for before control leaves.
const RUN: usize = 8;
/// Below this no entry points: the vectors and the header fill it.
const FIRST_CODE: u32 = 0x200;

/// Runs discovery and the recogniser in turns until no hint is added or table shrunk, and
/// returns the tables found as hints. `computed` holds the targets a profile run saw
/// computed jumps and calls take, by site: a site no table explains follows those, and a
/// target its table leaves out becomes a function entry.
pub(crate) fn fixpoint(
    bytes: &[u8],
    vectors: &[u32; 64],
    computed: &BTreeMap<Address, BTreeSet<Address>>,
) -> Hints {
    let mut hints = Hints::default();
    let mut tried = BTreeSet::new();
    for _ in 0..MAX_ROUNDS {
        let mut discovery = Discovery::new(bytes, vectors, &hints);
        discovery.run();
        let recogniser = Recogniser::new(bytes, &discovery.instructions, &hints);
        // Code found since a table was sized may end it sooner: tables only shrink, so the
        // loop ends.
        let shrunk: Vec<(usize, u32)> = hints
            .jump_tables
            .iter()
            .enumerate()
            .filter_map(|(position, table)| {
                let again = recogniser.recognise(table.site?)?;
                (again.address == table.address && again.count < table.count)
                    .then_some((position, again.count))
            })
            .collect();
        let mut added = Vec::new();
        let mut indirect = Vec::new();
        for site in &discovery.unresolved {
            if !tried.insert(site.site) {
                continue;
            }
            let Some(table) = recogniser.recognise(site.site) else {
                if let Some(targets) = computed.get(&site.site) {
                    indirect.push(IndirectHint {
                        site: site.site,
                        targets: targets.iter().copied().collect(),
                    });
                }
                continue;
            };
            // A second site reading a known table resolves through the first one's hint.
            let known = hints.jump_tables.iter().chain(&added);
            if !known
                .into_iter()
                .any(|other| other.address == table.address)
            {
                added.push(table);
            }
        }
        // A table sized short of an entry the run took: that target gets code of its own.
        // Sites still unresolved this round wait for the hints just made.
        let unresolved: BTreeSet<Address> =
            discovery.unresolved.iter().map(|site| site.site).collect();
        let entries: BTreeSet<Address> = computed
            .iter()
            .filter(|&(site, _)| {
                discovery.instructions.contains_key(site) && !unresolved.contains(site)
            })
            .flat_map(|(_, targets)| targets)
            .filter(|&target| {
                !discovery.instructions.contains_key(target) && hints.function(*target).is_none()
            })
            .copied()
            .collect();
        drop(discovery);
        if shrunk.is_empty() && added.is_empty() && indirect.is_empty() && entries.is_empty() {
            break;
        }
        hints.indirect.extend(indirect);
        hints
            .functions
            .extend(entries.into_iter().map(|address| FunctionHint {
                address,
                noreturn: false,
            }));
        for (position, count) in shrunk {
            hints.jump_tables[position].count = count;
        }
        hints.jump_tables.extend(added);
    }
    hints
}

/// The decoded code of one discovery round and what it says about where tables end.
struct Recogniser<'a> {
    bytes: &'a [u8],
    code: &'a BTreeMap<Address, Instruction>,
    /// Addresses the code names and the starts of known tables: no table of offsets or
    /// pointers runs past one, since the next table or data block starts there.
    labels: BTreeSet<Address>,
}

impl<'a> Recogniser<'a> {
    fn new(bytes: &'a [u8], code: &'a BTreeMap<Address, Instruction>, hints: &Hints) -> Self {
        let mut labels: BTreeSet<Address> = hints
            .jump_tables
            .iter()
            .map(|table| table.address)
            .collect();
        for (&at, instruction) in code {
            labels.extend(instruction.branch_target(at));
            for slot in 0..instruction.operands.len() {
                labels.extend(named_address(instruction, at, slot));
            }
        }
        Self {
            bytes,
            code,
            labels,
        }
    }

    /// Recognises the table the `jmp` or `jsr` at `site` dispatches through.
    fn recognise(&self, site: Address) -> Option<JumpTable> {
        let jump = self.code.get(&site)?;
        if !matches!(jump.mnemonic, Mnemonic::Jmp | Mnemonic::Jsr) {
            return None;
        }
        let before = self.preceding(site);
        let dispatch = match jump.operands[0]? {
            Operand::Ea(Ea::PcIndex { index, .. }) => {
                let table = pc_relative(jump, site, 0)?;
                self.pc_dispatch(&before, table, index.register)?
            }
            Operand::Ea(Ea::Indirect(register)) => register_dispatch(&before, register)?,
            _ => return None,
        };
        if !dispatch.table.is_even() || dispatch.table.get() >= self.rom_end() {
            return None;
        }
        let (address, heuristic) = if dispatch.kind == EntryKind::Long {
            self.long_start(dispatch.table)
        } else {
            (
                dispatch.table,
                self.heuristic_count(dispatch.table, dispatch.kind),
            )
        };
        let rest = &before[dispatch.scan_from.min(before.len())..];
        // A mask or compare only caps the index; the game may use fewer entries.
        let bound = bound(rest, dispatch.index, dispatch.kind.entry_bytes());
        let count = match (bound, heuristic) {
            (Some(bound), Some(heuristic)) => bound.min(heuristic),
            (bound, heuristic) => bound.or(heuristic)?,
        };
        Some(JumpTable {
            address,
            count,
            kind: dispatch.kind,
            site: Some(site),
            target: None,
        })
    }

    /// `jmp Tbl(pc,dx.w)`: word offsets read into `dx` just before, or a row of `bra.w`.
    fn pc_dispatch(
        &self,
        before: &[(Address, &Instruction)],
        table: Address,
        index: Register,
    ) -> Option<Dispatch> {
        let word_read = before.first().and_then(|&(at, previous)| {
            let read = pc_index_read(previous, at)?;
            (previous.mnemonic == Mnemonic::Move
                && previous.size == Some(Size::Word)
                && read.table == table
                && read.destination == Some(index))
            .then_some(read.index)
        });
        match word_read {
            Some(read_index) => Some(Dispatch {
                table,
                kind: EntryKind::WordOffset,
                index: read_index,
                scan_from: 1,
            }),
            None if tables::word_at(self.bytes, table) == Some(BRA_W) => Some(Dispatch {
                table,
                kind: EntryKind::BraW,
                index,
                scan_from: 0,
            }),
            None => None,
        }
    }

    /// The straight-line instructions before `site`, nearest first.
    fn preceding(&self, site: Address) -> Vec<(Address, &'a Instruction)> {
        let mut found = Vec::new();
        let mut at = site;
        while found.len() < LOOKBACK {
            let Some((&start, instruction)) = self.code.range(..at).next_back() else {
                break;
            };
            if start.wrapping_add(instruction.length()) != at || !falls_through(instruction) {
                break;
            }
            found.push((start, instruction));
            at = start;
        }
        found
    }

    /// Counts entries while their targets look like code; word and `bra.w` tables also end
    /// where the nearest target after them begins, since they sit just before that code.
    fn heuristic_count(&self, table: Address, kind: EntryKind) -> Option<u32> {
        let mut limit = self.rom_end();
        let mut count = 0;
        while count < MAX_ENTRIES {
            let entry = table.wrapping_add(count * kind.entry_bytes());
            if entry.get() >= limit {
                break;
            }
            // A row of bra.w is itself code; any other table ends at decoded code or at the
            // next address the code names.
            if count > 0
                && kind != EntryKind::BraW
                && (self.covered_by_code(entry) || self.labels.contains(&entry))
            {
                break;
            }
            let Some(target) = self.entry_target(table, kind, count) else {
                break;
            };
            // An offset or pointer into the entries read so far is data: decoding it would
            // cover the table with code and shrink it in the next round.
            let read = entry.wrapping_add(kind.entry_bytes());
            let into_table = kind != EntryKind::BraW && (table..read).contains(&target);
            if target == table || into_table || !self.plausible_target(target) {
                break;
            }
            if kind != EntryKind::Long && target > table {
                limit = limit.min(target.get());
            }
            count += 1;
        }
        (count > 0).then_some(count)
    }

    /// Long tables read as `Tbl-4(pc,dn.w)` start one entry later: skips one leading entry
    /// that is not a code pointer.
    fn long_start(&self, table: Address) -> (Address, Option<u32>) {
        let first = self.entry_target(table, EntryKind::Long, 0);
        let table = if first.is_some_and(|target| self.decodes_at(target)) {
            table
        } else {
            table.wrapping_add(4)
        };
        (table, self.heuristic_count(table, EntryKind::Long))
    }

    /// Where entry `index` leads: for `bra.w` rows, the branch's target rather than the row.
    fn entry_target(&self, table: Address, kind: EntryKind, index: u32) -> Option<Address> {
        let target = tables::entry_target(self.bytes, table, kind, index)?;
        if kind != EntryKind::BraW {
            return Some(target);
        }
        (tables::word_at(self.bytes, target) == Some(BRA_W))
            .then(|| tables::bra_w_target(self.bytes, target))
            .flatten()
    }

    /// Whether the code at `target` looks like code: it does not start inside a decoded
    /// instruction, and up to `RUN` instructions decode without exception opcodes or bad
    /// branch targets before control leaves or joins decoded code.
    fn plausible_target(&self, target: Address) -> bool {
        if !self.decodes_at(target)
            || (!self.code.contains_key(&target) && self.covered_by_code(target))
        {
            return false;
        }
        let mut at = target;
        for _ in 0..RUN {
            if at != target && self.code.contains_key(&at) {
                return true;
            }
            let Ok(instruction) = decode_at(self.bytes, at.get() as usize) else {
                return false;
            };
            if matches!(
                instruction.mnemonic,
                Mnemonic::Illegal
                    | Mnemonic::LineA
                    | Mnemonic::LineF
                    | Mnemonic::Reset
                    | Mnemonic::Stop
                    | Mnemonic::Trapv
            ) {
                return false;
            }
            if let Some(branch) = instruction.branch_target(at)
                && (!branch.is_even() || branch.get() >= self.rom_end())
            {
                return false;
            }
            if !falls_through(&instruction) {
                return true;
            }
            at = at.wrapping_add(instruction.length());
        }
        true
    }

    fn decodes_at(&self, target: Address) -> bool {
        target.is_even()
            && (FIRST_CODE..self.rom_end()).contains(&target.get())
            && decode_at(self.bytes, target.get() as usize).is_ok()
    }

    /// Whether `address` lies inside an instruction already decoded, its start included.
    fn covered_by_code(&self, address: Address) -> bool {
        self.code
            .range(..=address)
            .next_back()
            .is_some_and(|(start, instruction)| address < start.wrapping_add(instruction.length()))
    }

    fn rom_end(&self) -> u32 {
        // The ROM loader caps images at 4 MiB.
        self.bytes.len() as u32
    }
}

/// A dispatch idiom: where its table starts, how entries are encoded, the register that
/// indexes it, and where the instructions before the table read start.
#[derive(Debug)]
struct Dispatch {
    table: Address,
    kind: EntryKind,
    index: Register,
    scan_from: usize,
}

/// The register idioms: `movea.l Tbl(pc,dn.w),an`, `lea Tbl,am` + `movea.l (am,dn.w),an`,
/// and `lea Tbl,an` + `move.w (an,dn.w),dn` + `lea (an,dn.w),an`.
fn register_dispatch(before: &[(Address, &Instruction)], register: u8) -> Option<Dispatch> {
    let target = Register::Address(register);
    let (position, &(at, loader)) = before
        .iter()
        .enumerate()
        .find(|(_, (_, instruction))| writes(instruction, target))?;
    let rest = &before[position + 1..];
    match (loader.mnemonic, loader.operands[0]?) {
        (Mnemonic::Movea, Operand::Ea(Ea::PcIndex { .. })) => {
            let read = pc_index_read(loader, at)?;
            Some(Dispatch {
                table: read.table,
                kind: EntryKind::Long,
                index: read.index,
                scan_from: position + 1,
            })
        }
        (
            Mnemonic::Movea,
            Operand::Ea(Ea::Index {
                register: base,
                displacement: 0,
                index,
            }),
        ) if loader.size == Some(Size::Long) => Some(Dispatch {
            table: loaded_address(rest, base)?,
            kind: EntryKind::Long,
            index: index.register,
            scan_from: position + 1,
        }),
        (
            Mnemonic::Lea,
            Operand::Ea(Ea::Index {
                register: base,
                displacement: 0,
                index,
            }),
        ) if base == register => {
            // The offset read, move.w (an,dn.w),dn, is the nearest write to dn.
            let Register::Data(data) = index.register else {
                return None;
            };
            let (read_position, &(_, read)) = rest
                .iter()
                .enumerate()
                .find(|(_, (_, instruction))| writes(instruction, index.register))?;
            let reads_table = read.mnemonic == Mnemonic::Move
                && read.size == Some(Size::Word)
                && matches!(
                    read.operands,
                    [
                        Some(Operand::Ea(Ea::Index {
                            register: read_base,
                            displacement: 0,
                            index: read_index,
                        })),
                        Some(Operand::Ea(Ea::DataRegister(destination))),
                    ] if read_base == register
                        && destination == data
                        && read_index.register == index.register
                );
            if !reads_table {
                return None;
            }
            Some(Dispatch {
                table: loaded_address(rest, register)?,
                kind: EntryKind::WordOffset,
                index: index.register,
                scan_from: position + 1 + read_position + 1,
            })
        }
        _ => None,
    }
}

/// The address the nearest write to `an` put there, when that write is `lea Tbl,an`.
fn loaded_address(before: &[(Address, &Instruction)], register: u8) -> Option<Address> {
    let &(at, loader) = before
        .iter()
        .find(|(_, instruction)| writes(instruction, Register::Address(register)))?;
    if loader.mnemonic != Mnemonic::Lea {
        return None;
    }
    named_address(loader, at, 0)
}

/// What an instruction reading `Tbl(pc,dn.w)` as its source reads.
#[derive(Debug)]
struct PcIndexRead {
    table: Address,
    index: Register,
    destination: Option<Register>,
}

fn pc_index_read(instruction: &Instruction, at: Address) -> Option<PcIndexRead> {
    let Some(Operand::Ea(Ea::PcIndex { index, .. })) = instruction.operands[0] else {
        return None;
    };
    let destination = match instruction.operands[1] {
        Some(Operand::Ea(Ea::DataRegister(register))) => Some(Register::Data(register)),
        Some(Operand::Ea(Ea::AddressRegister(register))) => Some(Register::Address(register)),
        _ => None,
    };
    Some(PcIndexRead {
        table: pc_relative(instruction, at, 0)?,
        index: index.register,
        destination,
    })
}

/// The ROM or RAM address the operand in `slot` names: PC-relative or absolute.
fn named_address(instruction: &Instruction, at: Address, slot: usize) -> Option<Address> {
    match instruction.operands[slot]? {
        Operand::Ea(Ea::PcDisplacement(_) | Ea::PcIndex { .. }) => {
            pc_relative(instruction, at, slot)
        }
        Operand::Ea(Ea::AbsoluteLong(value)) => Some(Address::new(value)),
        // The CPU sign-extends a short absolute address.
        Operand::Ea(Ea::AbsoluteShort(value)) => Some(Address::new(value as i16 as u32)),
        _ => None,
    }
}

/// The address a PC-relative operand in `slot` names.
fn pc_relative(instruction: &Instruction, at: Address, slot: usize) -> Option<Address> {
    let base = instruction.extension_address(at, slot);
    match instruction.operands[slot]? {
        Operand::Ea(Ea::PcDisplacement(displacement)) => Some(base.offset(i32::from(displacement))),
        Operand::Ea(Ea::PcIndex { displacement, .. }) => Some(base.offset(i32::from(displacement))),
        _ => None,
    }
}

fn falls_through(instruction: &Instruction) -> bool {
    !matches!(
        instruction.mnemonic,
        Mnemonic::Bra
            | Mnemonic::Jmp
            | Mnemonic::Rts
            | Mnemonic::Rte
            | Mnemonic::Rtr
            | Mnemonic::Illegal
            | Mnemonic::LineA
            | Mnemonic::LineF
    )
}

/// Whether `instruction` may change `register`; MOVEM to registers counts as changing all.
fn writes(instruction: &Instruction, register: Register) -> bool {
    if matches!(
        instruction.mnemonic,
        Mnemonic::Cmp
            | Mnemonic::Cmpa
            | Mnemonic::Cmpi
            | Mnemonic::Cmpm
            | Mnemonic::Tst
            | Mnemonic::Btst
            | Mnemonic::Bcc(_)
            | Mnemonic::Bsr
            | Mnemonic::Jsr
            | Mnemonic::Pea
            | Mnemonic::Nop
    ) {
        return false;
    }
    let names = |operand: Option<Operand>| match (operand, register) {
        (Some(Operand::Ea(Ea::DataRegister(a))), Register::Data(b))
        | (Some(Operand::Ea(Ea::AddressRegister(a))), Register::Address(b)) => a == b,
        (Some(Operand::RegisterList(_)), _) => true,
        _ => false,
    };
    // Post-increment and pre-decrement change the address register too.
    let steps_register = instruction.operands.iter().flatten().any(|operand| {
        matches!(
            (operand, register),
            (Operand::Ea(Ea::PostIncrement(a) | Ea::PreDecrement(a)), Register::Address(b))
                if *a == b
        )
    });
    if steps_register {
        return true;
    }
    match (instruction.mnemonic, instruction.operands) {
        (Mnemonic::Exg, [first, second]) => names(first) || names(second),
        // DBcc counts its data register down.
        (Mnemonic::Dbcc(_), [first, _]) | (_, [first, None]) => names(first),
        (_, [_, second]) => names(second),
    }
}

/// The entry count `andi` or `cmp` + Bcc on the index allows before the table read, scaled
/// by the `add dn,dn` and `lsl #k,dn` between them.
fn bound(before: &[(Address, &Instruction)], index: Register, entry_bytes: u32) -> Option<u32> {
    let Register::Data(data) = index else {
        return None;
    };
    let entries = |max: u32| {
        let count = max / entry_bytes + 1;
        (count <= MAX_ENTRIES).then_some(count)
    };
    let mut shift = 0;
    for (position, &(_, instruction)) in before.iter().enumerate() {
        let on_index = matches!(
            instruction.operands[1],
            Some(Operand::Ea(Ea::DataRegister(register))) if register == data
        );
        match (instruction.mnemonic, instruction.operands[0]) {
            (Mnemonic::Add, Some(Operand::Ea(Ea::DataRegister(source))))
                if on_index && source == data =>
            {
                shift += 1;
            }
            (Mnemonic::Lsl | Mnemonic::Asl, Some(Operand::Quick(count))) if on_index => {
                shift += u32::from(count.unsigned_abs());
            }
            (Mnemonic::Andi, Some(Operand::Ea(Ea::Immediate(mask)))) if on_index => {
                return entries(mask.checked_shl(shift)?);
            }
            (Mnemonic::Cmp | Mnemonic::Cmpi, Some(Operand::Ea(Ea::Immediate(limit))))
                if on_index =>
            {
                // The branch is the instruction after the compare, nearer the site.
                let branch = before.get(position.checked_sub(1)?)?.1;
                let Mnemonic::Bcc(condition) = branch.mnemonic else {
                    return None;
                };
                let max = match condition {
                    Condition::Higher
                    | Condition::LowerOrSame
                    | Condition::Greater
                    | Condition::LessOrEqual => limit,
                    Condition::CarryClear
                    | Condition::CarrySet
                    | Condition::GreaterOrEqual
                    | Condition::Less => limit.checked_sub(1)?,
                    _ => return None,
                };
                return entries(max.checked_shl(shift)?);
            }
            (Mnemonic::Ext, _) => {}
            _ if writes(instruction, index) => return None,
            _ => {}
        }
    }
    None
}
