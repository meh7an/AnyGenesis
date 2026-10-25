//! Control-flow discovery: functions, basic blocks and jump tables.
//!
//! [`analyze`] descends from the reset and exception vectors and from the config's hints,
//! decoding every reachable instruction, then cuts the code into blocks and assigns them to
//! functions. What it cannot follow is reported, never guessed. [`FlagLiveness`] then
//! finds the flags something reads at every instruction of the result.

mod blocks;
mod discover;
mod liveness;
mod model;
mod report;
mod tables;

use anygenesis_gamecfg::GameConfig;
use anygenesis_rom::Rom;

pub use liveness::{FlagLiveness, FlagStats, FlagSummary, LivenessOptions};
pub use model::{
    Analysis, Block, Call, CallKind, Diagnostic, Edge, EdgeKind, Function, Origin, ResolvedTable,
    Source, Suggestion, TailJump, Terminator, UnresolvedSite,
};
pub use report::Report;

/// Why an analysis did not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AnalysisError {
    #[error("the config is for the ROM with SHA-1 {config}, but this ROM's SHA-1 is {rom}")]
    Sha1Mismatch { config: String, rom: String },
}

/// Discovers the code of `rom`, guided by `config`.
///
/// # Errors
/// Returns [`AnalysisError::Sha1Mismatch`] when `config` belongs to another ROM image.
pub fn analyze(rom: &Rom, config: &GameConfig) -> Result<Analysis, AnalysisError> {
    let sha1 = rom.sha1().to_string();
    if sha1 != config.sha1 {
        return Err(AnalysisError::Sha1Mismatch {
            config: config.sha1.clone(),
            rom: sha1,
        });
    }
    let mut discovery = discover::Discovery::new(rom.bytes(), rom.vectors().entries(), config);
    discovery.run();
    let mut blocks = blocks::build_blocks(
        &discovery.instructions,
        &discovery.effects,
        &discovery.leaders,
    );
    let functions = blocks::build_functions(&mut blocks, discovery.entries, config);
    let mut diagnostics = discovery.diagnostics;
    diagnostics.extend(blocks::unowned_blocks(&blocks));
    discovery.unresolved.sort_by_key(|site| site.site);
    Ok(Analysis {
        sha1,
        // The ROM loader caps images at 4 MiB.
        rom_size: rom.bytes().len() as u32,
        instructions: discovery.instructions,
        blocks,
        functions,
        jump_tables: discovery.tables,
        unresolved: discovery.unresolved,
        diagnostics,
    })
}
