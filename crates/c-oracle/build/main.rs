//! Emits and compiles the translated programs the tests run, and writes the Rust items
//! that reach them (`programs.rs` in OUT_DIR, included by src/programs.rs):
//! - the synthetic program (src/synthetic.rs), always;
//! - a SingleStepTests sample, when testdata/ holds the suite (`cargo xtask fetch-sst`);
//! - Sonic 1, when `ANYGENESIS_ROM` names the ROM.
//!
//! Function programs are emitted with cycle accounting and a check point at every block
//! start: the hook is how the tests cap runs that never end.

#[path = "../src/synthetic.rs"]
mod synthetic;

mod compile;
mod sst;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anygenesis_analysis::{Analysis, LivenessOptions, analyze};
use anygenesis_codegen_c::{CheckPoints, EmitOptions};
use anygenesis_recomp::build;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = manifest.join("../..");
    println!("cargo::rerun-if-changed=build");
    println!("cargo::rerun-if-changed=src/synthetic.rs");

    let mut items = String::new();
    let synthetic = synthetic::analysis();
    let prefix = "syn_";
    compile_program(&synthetic, prefix, &out.join("synthetic"), "ag_synthetic");
    items.push_str(&table_items(prefix, "synthetic_table"));
    items.push_str(&sonic1(&out));
    items.push_str(&sst::translate_sample(&workspace, &out));
    fs::write(out.join("programs.rs"), items).expect("programs.rs written");
}

/// The options of every function program.
fn function_options(prefix: &str) -> EmitOptions {
    EmitOptions {
        cycles: true,
        check_points: CheckPoints::BlockStarts,
        symbol_prefix: prefix.to_owned(),
        ..EmitOptions::default()
    }
}

/// Emits `analysis` into `directory` and compiles it into the static library `library`.
fn compile_program(analysis: &Analysis, prefix: &str, directory: &Path, library: &str) {
    let output = build::emit_program(
        analysis,
        LivenessOptions::default(),
        &function_options(prefix),
    );
    let sources = build::write_output(&output, directory).unwrap_or_else(|error| panic!("{error}"));
    compile::library(library, directory, &sources);
}

/// The extern declarations of a program's address table and a function `name` returning it.
fn table_items(prefix: &str, name: &str) -> String {
    format!(
        "unsafe extern \"C\" {{\n    static {prefix}ag_table: anygenesis_abi::AgEntry;\n    \
         static {prefix}ag_table_size: u32;\n}}\n\n\
         /// The program's address table.\n\
         pub fn {name}() -> &'static [anygenesis_abi::AgEntry] {{\n    \
         // SAFETY: the build script linked the program, whose table holds\n    \
         // `{prefix}ag_table_size` rows from `{prefix}ag_table` on, and never changes.\n    \
         unsafe {{\n        std::slice::from_raw_parts(\n            \
         &raw const {prefix}ag_table,\n            {prefix}ag_table_size as usize,\n        \
         )\n    }}\n}}\n\n"
    )
}

/// Translates Sonic 1 when `ANYGENESIS_ROM` is set.
fn sonic1(out: &Path) -> String {
    println!("cargo::rerun-if-env-changed=ANYGENESIS_ROM");
    let config_path = build::config_path("sonic1").unwrap_or_else(|error| panic!("{error}"));
    println!("cargo::rerun-if-changed={}", config_path.display());
    let Some(path) = env::var_os("ANYGENESIS_ROM") else {
        return "/// The SHA-1 of the translated Sonic 1 ROM; `None` without `ANYGENESIS_ROM`.\n\
                pub const SONIC1_SHA1: Option<&str> = None;\n\n\
                /// Sonic 1's address table, when translated.\n\
                pub fn sonic1_table() -> Option<&'static [anygenesis_abi::AgEntry]> {\n    \
                None\n}\n\n"
            .to_owned();
    };
    println!("cargo::rerun-if-changed={}", Path::new(&path).display());
    let rom =
        build::load_rom(Path::new(&path)).unwrap_or_else(|error| panic!("ANYGENESIS_ROM: {error}"));
    let config = build::load_config(&config_path).unwrap_or_else(|error| panic!("{error}"));
    let analysis = analyze(&rom, &config).unwrap_or_else(|error| panic!("{error}"));
    compile_program(&analysis, "", &out.join("sonic1"), "ag_sonic1");
    let table = table_items("", "sonic1_table_rows");
    format!(
        "{table}/// The SHA-1 of the translated Sonic 1 ROM; `None` without `ANYGENESIS_ROM`.\n\
         pub const SONIC1_SHA1: Option<&str> = Some(\"{}\");\n\n\
         /// Sonic 1's address table, when translated.\n\
         pub fn sonic1_table() -> Option<&'static [anygenesis_abi::AgEntry]> {{\n    \
         Some(sonic1_table_rows())\n}}\n\n",
        rom.sha1()
    )
}
