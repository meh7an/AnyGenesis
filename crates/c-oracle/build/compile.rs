//! Compiling emitted C into a static library linked into the crate.

use std::path::{Path, PathBuf};

use anygenesis_recomp::build;

/// Compiles `sources` (which include headers from `directory`) the way the game build does,
/// and links the static library `name`.
pub fn library(name: &str, directory: &Path, sources: &[PathBuf]) {
    build::compile(name, directory, sources).unwrap_or_else(|error| panic!("{error}"));
}
