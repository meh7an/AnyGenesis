//! Per-game TOML configuration, keyed on the ROM SHA-1.
//!
//! A config holds addresses, hints and table sizes only: the analysis finds everything else.

mod config;
mod error;
mod location;
mod parse;

pub use config::{
    DataRegion, EntryKind, FunctionHint, GameConfig, IndirectHint, JumpTable, TargetKind,
    UnreachedReason, UnreachedRegion,
};
pub use error::{ConfigError, Problem, ProblemKind};
pub use location::Location;
