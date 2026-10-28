use std::collections::{BTreeMap, BTreeSet};

use anygenesis_m68k::{DecodeError, Instruction};
use anygenesis_shared::Address;

use crate::{EntryKind, Report, TargetKind};

/// What control-flow discovery found in one ROM image.
///
/// Every map is keyed by address, so iteration follows ROM order. Edges and calls may name
/// targets without a block (outside the ROM, in RAM, undecodable); those are also listed in
/// [`Analysis::diagnostics`].
#[derive(Debug, Clone)]
pub struct Analysis {
    /// Lowercase hex SHA-1 of the analysed image.
    pub sha1: String,
    /// Size of the analysed image in bytes.
    pub rom_size: u32,
    /// Every instruction decoded as reachable code, by start address.
    pub instructions: BTreeMap<Address, Instruction>,
    /// Basic blocks by start address. No instruction belongs to two blocks.
    pub blocks: BTreeMap<Address, Block>,
    /// Functions by entry address.
    pub functions: BTreeMap<Address, Function>,
    /// The jump tables used, with their decoded targets.
    pub jump_tables: Vec<ResolvedTable>,
    /// Indirect jumps and calls nothing explains, each with a suggested hint.
    pub unresolved: Vec<UnresolvedSite>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    /// Returns the blocks reached from more than one function entry.
    pub fn shared_blocks(&self) -> impl Iterator<Item = &Block> {
        self.blocks
            .values()
            .filter(|block| block.functions.len() > 1)
    }

    /// Returns whether a jump from inside `function` to `target` leaves it: `target` is
    /// another function's entry.
    pub fn is_tail_jump(&self, function: Address, target: Address) -> bool {
        target != function && self.functions.contains_key(&target)
    }

    /// Returns a Markdown report of the results, for reading and for hint iteration.
    pub fn report(&self) -> Report<'_> {
        Report::new(self)
    }
}

/// A straight run of instructions entered only at its start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub start: Address,
    /// One past the last byte of the last instruction.
    pub end: Address,
    /// The start of the last instruction.
    pub last: Address,
    pub terminator: Terminator,
    /// Where control goes after the block, calls excluded.
    pub successors: Vec<Edge>,
    /// Calls made inside the block, in address order. Calls do not end a block unless the
    /// callee never returns.
    pub calls: Vec<Call>,
    /// Entries of every function that reaches this block; more than one marks it shared.
    pub functions: BTreeSet<Address>,
}

/// Why a block ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Terminator {
    /// The next instruction starts another block.
    Fallthrough,
    /// RTS.
    Return,
    /// RTE.
    ReturnFromException,
    /// RTR.
    ReturnRestoringCcr,
    /// BRA, or JMP to a fixed address.
    Jump,
    /// Bcc: the taken edge, then the fallthrough edge.
    Conditional,
    /// DBcc: the loop edge, then the fallthrough edge.
    DecrementBranch,
    /// JMP through a register or table; its successors come from a table or a hint, and
    /// it has none when unresolved.
    IndirectJump,
    /// TRAP: the handler runs as a call, then execution falls through.
    Trap,
    /// STOP: execution resumes at the next instruction after an interrupt.
    Stop,
    /// ILLEGAL, line A or line F: the exception handler never returns here.
    Exception,
    /// A call to a function hinted as `noreturn`.
    NoReturnCall,
    /// The code runs into undecodable bytes, a data region or the end of the ROM.
    Invalid,
}

impl Terminator {
    /// Returns whether execution can continue at the next instruction.
    pub const fn falls_through(self) -> bool {
        matches!(
            self,
            Self::Fallthrough | Self::Conditional | Self::DecrementBranch | Self::Trap | Self::Stop
        )
    }
}

/// A control-flow edge out of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    pub target: Address,
    pub kind: EdgeKind,
}

/// How an edge is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EdgeKind {
    /// Into the next instruction.
    Fallthrough,
    /// BRA or JMP to a fixed address.
    Jump,
    /// The taken side of Bcc or DBcc.
    Taken,
    /// An entry of the jump table at `table`.
    Table { table: Address },
    /// A target listed by an indirect hint.
    Hinted,
}

/// A call inside a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The address of the calling instruction.
    pub site: Address,
    pub kind: CallKind,
    /// Every callee found, bad targets included (they are diagnosed); empty for an
    /// unresolved indirect call.
    pub targets: Vec<Address>,
}

/// The instruction that makes a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CallKind {
    Bsr,
    Jsr,
    /// TRAP #n calls the handler in vector 32 + n.
    Trap,
}

/// A function: an entry and the blocks reachable from it without leaving through a call
/// or a tail jump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub entry: Address,
    /// Why the entry is a function, call sites excluded.
    pub origins: Vec<Origin>,
    /// Addresses of the instructions that call it.
    pub callers: BTreeSet<Address>,
    pub blocks: BTreeSet<Address>,
    /// Jumps from this function into other functions' entries.
    pub tail_jumps: Vec<TailJump>,
    /// Hinted as never returning.
    pub noreturn: bool,
}

/// Why an address is a function entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Origin {
    /// The reset vector.
    Reset,
    /// Exception vector `n` (2 to 47).
    Vector(u8),
    /// A function hint.
    Hint,
    /// An entry of a jump table whose targets are functions.
    Table { table: Address },
    /// A target of the indirect hint for this site.
    Indirect { site: Address },
}

/// A jump from inside one function into another function's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TailJump {
    /// The last instruction of the jumping block.
    pub site: Address,
    pub target: Address,
}

/// A hinted jump table and what its entries lead to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTable {
    pub address: Address,
    pub kind: EntryKind,
    pub count: u32,
    /// The indirect jump or call that reads it, `None` when no site was found.
    pub site: Option<Address>,
    pub target: TargetKind,
    /// The decoded target of every entry, in table order.
    pub targets: Vec<Address>,
}

/// An indirect jump or call with no hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSite {
    pub site: Address,
    /// JSR rather than JMP.
    pub is_call: bool,
    pub suggestion: Suggestion,
}

/// The hint the report proposes for an unresolved site.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Suggestion {
    /// A PC-relative indexed jump: the table address is known, its size is not.
    JumpTable {
        address: Address,
        kind: EntryKind,
        /// A heuristic guess that must be checked by hand.
        count_guess: Option<u32>,
    },
    /// A jump through a register: list the targets by hand.
    Indirect,
}

/// Where a target came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Source {
    /// The instruction at this address.
    Instruction(Address),
    /// Exception vector `n` (1 is reset).
    Vector(u8),
    /// A hint.
    Hint,
}

/// Something the analysis could not follow or that needs a human look.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Diagnostic {
    /// An instruction starts inside another one.
    Overlap { first: Address, second: Address },
    /// A target at an odd address.
    OddTarget { source: Source, target: Address },
    /// A target in RAM, which static code cannot follow.
    RamTarget { source: Source, target: Address },
    /// A target past the end of the ROM and outside RAM.
    OutOfRomTarget { source: Source, target: Address },
    /// A target inside a hinted data region.
    DataTarget {
        source: Source,
        target: Address,
        region: Address,
    },
    /// The bytes at a reachable address are not an instruction.
    DecodeFailed {
        address: Address,
        error: DecodeError,
    },
    /// Execution continues past the instruction at `address` into a data region.
    FallsIntoData { address: Address, region: Address },
    /// Execution continues past the instruction at `address` beyond the end of the ROM.
    RunsOffRom { address: Address },
    /// Several vectors share one handler.
    SharedHandler { target: Address, vectors: Vec<u8> },
    /// A `bra_w` table entry that is not a `bra.w` instruction.
    NotBraW { table: Address, entry: Address },
    /// A table's bytes overlap a decoded instruction.
    TableOverlapsCode {
        table: Address,
        instruction: Address,
    },
    /// A jump table names a site that is not an indirect JMP or JSR.
    TableSiteNotIndirect { table: Address, site: Address },
    /// A block no function entry reaches (from a `target = "block"` table without a site).
    UnownedBlock { start: Address },
}
