//! Test-only oracles: the vendored Musashi core and disassembler behind a safe wrapper,
//! loaders for the SingleStepTests/m68000 and SingleStepTests/z80 per-instruction vectors,
//! and the vendored ymfm's YM2612.

pub mod musashi;
pub mod sst;
pub mod sst_z80;
pub mod ymfm;

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
