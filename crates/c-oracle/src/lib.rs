//! Translated C proven against the reference cores (test-only).
//!
//! The build script emits C for the synthetic program, a SingleStepTests sample and the ROM
//! `ANYGENESIS_ROM` names, compiles it with clang and links it into this crate ([`programs`]). The `ag_*` externs
//! the generated code calls are implemented in [`host`] over the bus in [`bus`]. A function
//! run starts the reference ([`reference`], the IR interpreter with Musashi in
//! [`lockstep`]) and the translated code from the same random state and compares where and
//! how both end ([`differential`]).

pub mod bus;
pub mod differential;
pub mod host;
pub mod lockstep;
pub mod programs;
pub mod random;
pub mod reference;
pub mod synthetic;
