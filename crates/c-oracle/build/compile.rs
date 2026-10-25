//! Compiling emitted C into a static library linked into the crate.

use std::{
    env,
    path::{Path, PathBuf},
};

/// Compiles `sources` (which include headers from `directory`) with clang-cl on MSVC
/// targets and clang elsewhere, as the game build will, and links the static library
/// `name`.
pub fn library(name: &str, directory: &Path, sources: &[PathBuf]) {
    let target = env::var("TARGET").expect("cargo sets TARGET");
    let mut build = cc::Build::new();
    if target.ends_with("msvc") {
        build.prefer_clang_cl_over_msvc(true);
    } else {
        build.compiler("clang");
    }
    let tool = build.get_compiler();
    assert!(
        tool.is_like_clang_cl() || tool.is_like_clang(),
        "translated C needs clang or clang-cl, found {}",
        tool.path().display()
    );
    build
        .std("c11")
        .include(directory)
        .files(sources)
        .compile(name);
}
