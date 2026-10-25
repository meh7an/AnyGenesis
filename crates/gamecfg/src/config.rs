use anygenesis_shared::Address;
use serde::Deserialize;

use crate::{ConfigError, Location, parse};

/// The hints for one ROM image.
///
/// The reset and interrupt vectors come from the ROM itself; a config only adds what the
/// analysis cannot find on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameConfig {
    /// Lowercase hex SHA-1 of the whole `.bin` image this config belongs to.
    pub sha1: String,
    /// Extra function entries, each also a root of the descent.
    pub functions: Vec<FunctionHint>,
    /// Jump tables, sized by hand.
    pub jump_tables: Vec<JumpTable>,
    /// Byte ranges that are never decoded as code.
    pub data_regions: Vec<DataRegion>,
    /// Explicit targets for indirect jumps and calls no table explains.
    pub indirect: Vec<IndirectHint>,
    /// Code the analysis is known not to reach, with the reason.
    pub unreached: Vec<UnreachedRegion>,
}

impl GameConfig {
    /// Parses and validates a config file's text.
    ///
    /// # Errors
    /// Returns [`ConfigError::Parse`] for malformed TOML, unknown keys and wrongly typed
    /// values, and [`ConfigError::Invalid`] with every problem found when the values break
    /// the schema's rules. Both carry line numbers.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        parse::parse(text)
    }

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

    /// Returns the unreached region holding `address`, if any.
    pub fn unreached_region(&self, address: Address) -> Option<&UnreachedRegion> {
        self.unreached
            .iter()
            .find(|region| region.contains(address))
    }
}

/// An address the analysis must treat as a function entry, `[[function]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionHint {
    pub address: Address,
    /// The function never returns to its caller, so the bytes after a call to it are not
    /// decoded as the caller's code.
    pub noreturn: bool,
    pub location: Location,
}

/// A table of jump targets, `[[jump_table]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub location: Location,
}

/// How a jump table encodes its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
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

    /// Returns the name the config file uses.
    pub const fn name(self) -> &'static str {
        match self {
            Self::WordOffset => "word_offset",
            Self::Long => "long",
            Self::BraW => "bra_w",
        }
    }
}

/// What the entries of a jump table lead to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TargetKind {
    /// Code inside the function that jumps through the table.
    Block,
    /// Separate functions, called or jumped to.
    Function,
}

/// Bytes that are data, `[[data]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRegion {
    pub start: Address,
    /// One past the last byte.
    pub end: Address,
    pub location: Location,
}

impl DataRegion {
    /// Returns whether the region holds `address`.
    pub fn contains(&self, address: Address) -> bool {
        (self.start..self.end).contains(&address)
    }
}

/// Code the analysis does not reach on purpose, `[[unreached]]`.
///
/// The analysis ignores these regions: validation checks every listed instruction the
/// analysis misses against them, and listings mark them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachedRegion {
    pub start: Address,
    /// One past the last byte.
    pub end: Address,
    pub reason: UnreachedReason,
    pub location: Location,
}

impl UnreachedRegion {
    /// Returns whether the region holds `address`.
    pub fn contains(&self, address: Address) -> bool {
        (self.start..self.end).contains(&address)
    }
}

/// Why code is never reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnreachedReason {
    /// Nothing in the ROM jumps to, calls or points at the code.
    Dead,
}

impl UnreachedReason {
    /// Returns the name the config file uses.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dead => "dead",
        }
    }
}

/// Explicit targets for one indirect jump or call, `[[indirect]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndirectHint {
    /// The address of the `jmp` or `jsr` instruction.
    pub site: Address,
    /// Every address it can reach: blocks for a `jmp`, functions for a `jsr`.
    pub targets: Vec<Address>,
    pub location: Location,
}
