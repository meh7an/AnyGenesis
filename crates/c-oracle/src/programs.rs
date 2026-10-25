//! The translated programs the build script linked in: the synthetic program always, the
//! SingleStepTests sample when testdata/ was fetched, Sonic 1 when `ANYGENESIS_ROM` was set.

use anygenesis_abi::AgFn;

/// One SingleStepTests case translated as a one-instruction function.
#[derive(Debug, Clone, Copy)]
pub struct SstCase {
    /// The suite file, as `ABCD.json.bin`.
    pub file: &'static str,
    /// The case's index in the file.
    pub index: usize,
    pub function: AgFn,
}

/// Cases of one file that were not translated, and why.
#[derive(Debug, Clone, Copy)]
pub struct SstExclusion {
    pub file: &'static str,
    pub reason: &'static str,
    pub count: usize,
}

include!(concat!(env!("OUT_DIR"), "/programs.rs"));
