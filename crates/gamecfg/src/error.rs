use std::fmt;

use anygenesis_shared::Address;

use crate::Location;

/// Why a config file was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The text is not TOML, or does not fit the schema's keys and types.
    #[error("{}", ParseText { location: *location, message })]
    Parse {
        /// Where the TOML parser stopped, when it says.
        location: Option<Location>,
        message: String,
    },
    /// The values break the schema's rules; every problem found is listed.
    #[error("{}", ProblemList(problems))]
    Invalid { problems: Vec<Problem> },
}

struct ParseText<'a> {
    location: Option<Location>,
    message: &'a str,
}

impl fmt::Display for ParseText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.location {
            Some(location) => write!(f, "{location}: {}", self.message),
            None => f.write_str(self.message),
        }
    }
}

struct ProblemList<'a>(&'a [Problem]);

impl fmt::Display for ProblemList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, problem) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{problem}")?;
        }
        Ok(())
    }
}

/// One rule a config value breaks, with the place of the offending value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub location: Location,
    pub kind: ProblemKind,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.kind)
    }
}

/// The rules a config value can break.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProblemKind {
    /// `sha1` is not 40 hex digits.
    BadSha1,
    /// The value does not fit the 68000's 24-bit address space.
    AddressOutOfRange { value: u32 },
    /// Code, word tables and long tables start at even addresses.
    OddAddress { address: Address },
    /// A jump table with no entries.
    ZeroCount,
    /// A data region whose end is not past its start.
    EmptyRegion { start: Address, end: Address },
    /// Two data regions share bytes.
    OverlappingRegions { other: Location },
    /// The same address is given twice where it must be unique.
    Duplicate { address: Address, first: Location },
    /// A code address lies inside a data region.
    InsideData { address: Address, region: Location },
    /// An `[[indirect]]` hint without targets.
    NoTargets,
}

impl fmt::Display for ProblemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSha1 => f.write_str("sha1 must be 40 hex digits"),
            Self::AddressOutOfRange { value } => {
                write!(f, "{value:#x} is outside the 24-bit address space")
            }
            Self::OddAddress { address } => write!(f, "{address} is odd"),
            Self::ZeroCount => f.write_str("a jump table needs at least one entry"),
            Self::EmptyRegion { start, end } => {
                write!(f, "data region end {end} is not past its start {start}")
            }
            Self::OverlappingRegions { other } => {
                write!(f, "data region overlaps the one at line {}", other.line)
            }
            Self::Duplicate { address, first } => {
                write!(f, "{address} is already given at line {}", first.line)
            }
            Self::InsideData { address, region } => {
                write!(
                    f,
                    "{address} lies in the data region at line {}",
                    region.line
                )
            }
            Self::NoTargets => f.write_str("an indirect hint needs at least one target"),
        }
    }
}
