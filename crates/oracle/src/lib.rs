//! Test-only CPU oracles: the vendored Musashi core and disassembler behind a safe wrapper,
//! and a loader for the SingleStepTests/m68000 per-instruction vectors.

pub mod musashi;
pub mod sst;

/// The 68000 programmer-visible registers, in the order SingleStepTests stores them.
///
/// A7 is not a field of its own: it is `usp` or `ssp`, depending on the S bit of `sr`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Registers {
    pub d: [u32; 8],
    pub a: [u32; 7],
    pub usp: u32,
    pub ssp: u32,
    pub sr: u32,
    pub pc: u32,
}
