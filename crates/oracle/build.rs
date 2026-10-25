//! Builds the vendored Musashi core: compiles and runs its `m68kmake` generator, which writes
//! the opcode handlers (`m68kops.c`, `m68kops.h`) from `m68k_in.c`, then compiles the core,
//! its disassembler and the Rust bridge with `cc`.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

/// Musashi's options are `#ifndef`-guarded in m68kconf.h, so command-line defines override
/// them without touching vendored files.
const MUSASHI_OPTIONS: [(&str, &str); 6] = [
    ("M68K_EMULATE_010", "M68K_OPT_OFF"),
    ("M68K_EMULATE_EC020", "M68K_OPT_OFF"),
    ("M68K_EMULATE_020", "M68K_OPT_OFF"),
    ("M68K_EMULATE_030", "M68K_OPT_OFF"),
    ("M68K_EMULATE_040", "M68K_OPT_OFF"),
    ("M68K_EMULATE_PMMU", "M68K_OPT_OFF"),
];

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let musashi = manifest_dir.join("../../vendor/musashi");
    let shim = manifest_dir.join("csrc/shim.c");

    println!("cargo:rerun-if-changed={}", musashi.display());
    println!("cargo:rerun-if-changed={}", shim.display());

    generate_opcode_handlers(&musashi, &out_dir);

    let mut build = cc::Build::new();
    build
        .files([
            musashi.join("m68kcpu.c"),
            musashi.join("m68kdasm.c"),
            musashi.join("softfloat/softfloat.c"),
            out_dir.join("m68kops.c"),
            shim,
        ])
        .include(&musashi)
        .include(&out_dir)
        .define("M68K_EMULATE_ADDRESS_ERROR", "M68K_OPT_ON")
        // Vendored code stays unmodified, so its warnings are not ours to fix.
        .warnings(false);
    for (name, value) in MUSASHI_OPTIONS {
        build.define(name, value);
    }
    build.compile("musashi");

    // The FPU code that m68kcpu.c always includes calls into libm.
    if env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|family| family == "unix") {
        println!("cargo:rustc-link-lib=m");
    }
}

/// Compiles `m68kmake` for the host and runs it, writing its output into `out_dir`.
fn generate_opcode_handlers(musashi: &Path, out_dir: &Path) {
    let host = env::var("HOST").expect("set by cargo");
    let compiler = cc::Build::new()
        .target(&host)
        .host(&host)
        .warnings(false)
        .cargo_metadata(false)
        .get_compiler();
    let exe_name = if cfg!(windows) {
        "m68kmake.exe"
    } else {
        "m68kmake"
    };
    let exe = out_dir.join(exe_name);

    let mut compile = compiler.to_command();
    compile.current_dir(out_dir).arg(musashi.join("m68kmake.c"));
    if compiler.is_like_msvc() {
        compile.arg(format!("/Fe{}", exe.display()));
    } else {
        compile.arg("-o").arg(&exe);
    }
    run(&mut compile, "compiling m68kmake");

    run(
        Command::new(&exe)
            .arg(out_dir)
            .arg(musashi.join("m68k_in.c")),
        "running m68kmake",
    );
}

/// Runs `command` with its output captured, because cargo parses a build script's stdout
/// for directives.
fn run(command: &mut Command, what: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{what}: {error}"));
    assert!(
        output.status.success(),
        "{what} failed with {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
