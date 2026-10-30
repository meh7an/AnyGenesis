//! Emits and compiles the translated programs the tests run, and writes the Rust items
//! that reach them (`programs.rs` in OUT_DIR, included by src/programs.rs):
//! - the synthetic program (src/synthetic.rs), always;
//! - a SingleStepTests sample, when testdata/ holds the suite (`cargo xtask fetch-sst`);
//! - the ROM `ANYGENESIS_ROM` names, when it is set; analysed with the profile
//!   `ANYGENESIS_PROFILE` names (a build's profile.txt) when that is set too.
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

use anygenesis_analysis::{Analysis, LivenessOptions, analyze, analyze_profiled};
use anygenesis_codegen_c::{CheckPoints, EmitOptions};
use anygenesis_recomp::build;
use anygenesis_rom::Rom;
use anygenesis_shared::Profile;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let workspace = manifest.join("../..");
    println!("cargo::rerun-if-changed=build");
    println!("cargo::rerun-if-changed=src/synthetic.rs");

    let mut items = String::new();
    let synthetic = synthetic::analysis();
    let image = Rom::from_bytes(synthetic::image()).expect("the synthetic image is a valid ROM");
    let prefix = "syn_";
    compile_program(
        &synthetic,
        &image,
        prefix,
        &out.join("synthetic"),
        "ag_synthetic",
    );
    items.push_str(&table_items(prefix, "synthetic_table"));
    items.push_str(&rom(&out));
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

/// Emits `analysis` of `rom` into `directory` and compiles it into the static library
/// `library`.
fn compile_program(analysis: &Analysis, rom: &Rom, prefix: &str, directory: &Path, library: &str) {
    let output = build::emit_program(
        analysis,
        rom,
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

/// Translates the ROM `ANYGENESIS_ROM` names, analysed from its vectors as game builds do
/// (following the profile `ANYGENESIS_PROFILE` names, if any), when it is set.
fn rom(out: &Path) -> String {
    println!("cargo::rerun-if-env-changed=ANYGENESIS_ROM");
    println!("cargo::rerun-if-env-changed=ANYGENESIS_PROFILE");
    let Some(path) = env::var_os("ANYGENESIS_ROM") else {
        return "/// The SHA-1 of the translated ROM; `None` without `ANYGENESIS_ROM`.\n\
                pub const ROM_SHA1: Option<&str> = None;\n\n\
                /// The translated ROM's address table, when there is one.\n\
                pub fn rom_table() -> Option<&'static [anygenesis_abi::AgEntry]> {\n    \
                None\n}\n\n"
            .to_owned();
    };
    println!("cargo::rerun-if-changed={}", Path::new(&path).display());
    let rom =
        build::load_rom(Path::new(&path)).unwrap_or_else(|error| panic!("ANYGENESIS_ROM: {error}"));
    let analysis = match env::var_os("ANYGENESIS_PROFILE") {
        Some(path) => {
            println!("cargo::rerun-if-changed={}", Path::new(&path).display());
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("ANYGENESIS_PROFILE: {error}"));
            let (profile, _) = Profile::parse(&text, rom.bytes().len() as u32)
                .unwrap_or_else(|error| panic!("ANYGENESIS_PROFILE: {error}"));
            analyze_profiled(&rom, &profile)
        }
        None => analyze(&rom),
    };
    compile_program(&analysis, &rom, "", &out.join("rom"), "ag_rom");
    let table = table_items("", "rom_table_rows");
    format!(
        "{table}/// The SHA-1 of the translated ROM; `None` without `ANYGENESIS_ROM`.\n\
         pub const ROM_SHA1: Option<&str> = Some(\"{}\");\n\n\
         /// The translated ROM's address table, when there is one.\n\
         pub fn rom_table() -> Option<&'static [anygenesis_abi::AgEntry]> {{\n    \
         Some(rom_table_rows())\n}}\n\n",
        rom.sha1()
    )
}
