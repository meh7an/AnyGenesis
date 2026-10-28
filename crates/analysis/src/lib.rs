//! Control-flow discovery: functions, basic blocks and jump tables.
//!
//! [`analyze`] descends from the reset and exception vectors, decoding every reachable
//! instruction, then cuts the code into blocks and assigns them to functions; [`analyze_with`]
//! also follows [`Hints`]. What it cannot follow is reported, never guessed. [`FlagLiveness`]
//! then finds the flags something reads at every instruction of the result.

mod blocks;
mod discover;
mod hints;
mod liveness;
mod model;
mod report;
mod tables;

use anygenesis_rom::Rom;

pub use hints::{DataRegion, EntryKind, FunctionHint, Hints, IndirectHint, JumpTable, TargetKind};
pub use liveness::{FlagLiveness, FlagStats, FlagSummary, LivenessOptions};
pub use model::{
    Analysis, Block, Call, CallKind, Diagnostic, Edge, EdgeKind, Function, Origin, ResolvedTable,
    Source, Suggestion, TailJump, Terminator, UnresolvedSite,
};
pub use report::Report;

/// Discovers the code of `rom` from its vectors alone, as game builds do.
pub fn analyze(rom: &Rom) -> Analysis {
    analyze_with(rom, &Hints::default())
}

/// Discovers the code of `rom` from its vectors and `hints`.
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
