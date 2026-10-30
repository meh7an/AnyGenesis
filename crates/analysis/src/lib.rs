//! Control-flow discovery: functions, basic blocks and jump tables.
//!
//! [`analyze`] descends from the reset and exception vectors, decoding every reachable
//! instruction, recognises the jump tables its indirect jumps read and descends again until
//! none is new, then cuts the code into blocks and assigns them to functions.
//! [`analyze_stage`] can stop at the vectors; [`analyze_profiled`] also follows the computed
//! jumps a profile run took; [`analyze_with`] follows [`Hints`] and nothing else. What it cannot follow is reported, never guessed. [`FlagLiveness`] then finds the
//! flags something reads at every instruction of the result.

mod blocks;
mod discover;
mod hints;
mod liveness;
mod model;
mod recognise;
mod report;
mod tables;

use std::collections::BTreeMap;

use anygenesis_rom::Rom;
use anygenesis_shared::Profile;

pub use hints::{DataRegion, EntryKind, FunctionHint, Hints, IndirectHint, JumpTable, TargetKind};
pub use liveness::{FlagLiveness, FlagStats, FlagSummary, LivenessOptions};
pub use model::{
    Analysis, Block, Call, CallKind, Diagnostic, Edge, EdgeKind, Function, Origin, ResolvedTable,
    Source, Suggestion, TailJump, Terminator, UnresolvedSite,
};
pub use report::Report;

/// How far discovery goes beyond the vectors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiscoveryStage {
    /// The descent from the vectors alone.
    Vectors,
    /// The descent, plus every jump table the recogniser finds behind an indirect jump.
    #[default]
    Static,
}

/// Discovers the code of `rom` from the ROM alone, as game builds do.
pub fn analyze(rom: &Rom) -> Analysis {
    analyze_stage(rom, DiscoveryStage::default())
}

/// Discovers the code of `rom` from the ROM alone, going as far as `stage`.
pub fn analyze_stage(rom: &Rom, stage: DiscoveryStage) -> Analysis {
    let hints = match stage {
        DiscoveryStage::Vectors => Hints::default(),
        DiscoveryStage::Static => {
            recognise::fixpoint(rom.bytes(), rom.vectors().entries(), &BTreeMap::new())
        }
    };
    analyze_with(rom, &hints)
}

/// Discovers the code of `rom` as [`analyze`] does, and also follows where `profile`, a run
/// of it, saw computed jumps and calls go: the code the run reached that way becomes
/// functions too.
pub fn analyze_profiled(rom: &Rom, profile: &Profile) -> Analysis {
    let hints = recognise::fixpoint(
        rom.bytes(),
        rom.vectors().entries(),
        profile.computed_targets(),
    );
    analyze_with(rom, &hints)
}

/// Discovers the code of `rom` from its vectors and `hints`, with no recogniser.
pub fn analyze_with(rom: &Rom, hints: &Hints) -> Analysis {
    let mut discovery = discover::Discovery::new(rom.bytes(), rom.vectors().entries(), hints);
    discovery.run();
    let mut blocks = blocks::build_blocks(
        &discovery.instructions,
        &discovery.effects,
        &discovery.leaders,
    );
    let functions = blocks::build_functions(&mut blocks, discovery.entries, hints);
    let mut diagnostics = discovery.diagnostics;
    diagnostics.extend(blocks::unowned_blocks(&blocks));
    discovery.unresolved.sort_by_key(|site| site.site);
    Analysis {
        sha1: rom.sha1().to_string(),
        // The ROM loader caps images at 4 MiB.
        rom_size: rom.bytes().len() as u32,
        instructions: discovery.instructions,
        blocks,
        functions,
        jump_tables: discovery.tables,
        unresolved: discovery.unresolved,
        diagnostics,
    }
}
