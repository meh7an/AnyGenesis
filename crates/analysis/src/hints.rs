use anygenesis_shared::Address;

/// Facts about a ROM the descent from its vectors cannot find on its own.
///
/// Game builds write none: the recogniser derives jump tables from the ROM itself, and every
/// address discovery misses is translated as stepped code. Written hints exist for synthetic
/// tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    /// Extra function entries, each also a root of the descent.
    pub functions: Vec<FunctionHint>,
    /// Jump tables with their sizes.
    pub jump_tables: Vec<JumpTable>,
    /// Byte ranges that are never decoded as code.
    pub data_regions: Vec<DataRegion>,
    /// Explicit targets for indirect jumps and calls no table explains.
    pub indirect: Vec<IndirectHint>,
}

impl Hints {
    /// Returns the function hint at `address`, if any.
    pub fn function(&self, address: Address) -> Option<&FunctionHint> {
        self.functions.iter().find(|hint| hint.address == address)
    }

    /// Returns the jump table that starts at `address`, if any.
    pub fn jump_table(&self, address: Address) -> Option<&JumpTable> {
        self.jump_tables
            .iter()
            .find(|table| table.address == address)
    }

    /// Returns the indirect-target hint for the instruction at `site`, if any.
    pub fn indirect(&self, site: Address) -> Option<&IndirectHint> {
        self.indirect.iter().find(|hint| hint.site == site)
    }

    /// Returns the data region holding `address`, if any.
    pub fn data_region(&self, address: Address) -> Option<&DataRegion> {
        self.data_regions
            .iter()
            .find(|region| region.contains(address))
    }
}

/// An address the analysis must treat as a function entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionHint {
    pub address: Address,
    /// The function never returns to its caller, so the bytes after a call to it are not
    /// decoded as the caller's code.
    pub noreturn: bool,
}

/// A table of jump targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpTable {
    /// The first byte of the table.
    pub address: Address,
    /// Number of entries.
    pub count: u32,
    pub kind: EntryKind,
    /// The indirect jump or call that reads the table, when known.
    pub site: Option<Address>,
    /// What the entries lead to; `None` lets the analysis decide from the site.
    pub target: Option<TargetKind>,
}

/// How a jump table encodes its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryKind {
    /// Signed 16-bit offsets from the start of the table, as `jmp Tbl(pc,dn.w)` reads them.
    WordOffset,
    /// 32-bit absolute pointers.
    Long,
    /// A row of 4-byte `bra.w` instructions; every entry is itself code.
    BraW,
}

impl EntryKind {
    /// Returns the size of one entry in bytes.
    pub const fn entry_bytes(self) -> u32 {
        match self {
            Self::WordOffset => 2,
            Self::Long | Self::BraW => 4,
        }
    }

    /// Returns the name reports use.
    pub const fn name(self) -> &'static str {
        match self {
            Self::WordOffset => "word_offset",
            Self::Long => "long",
            Self::BraW => "bra_w",
        }
    }
}

/// What the entries of a jump table lead to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TargetKind {
    /// Code inside the function that jumps through the table.
    Block,
    /// Separate functions, called or jumped to.
    Function,
}

/// Bytes that are data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRegion {
    pub start: Address,
    /// One past the last byte.
    pub end: Address,
}

impl DataRegion {
    /// Returns whether the region holds `address`.
    pub fn contains(&self, address: Address) -> bool {
        (self.start..self.end).contains(&address)
    }
}

/// Explicit targets for one indirect jump or call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndirectHint {
    /// The address of the `jmp` or `jsr` instruction.
    pub site: Address,
    /// Every address it can reach: blocks for a `jmp`, functions for a `jsr`.
    pub targets: Vec<Address>,
}
