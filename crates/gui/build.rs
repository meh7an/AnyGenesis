include!("../shared/version_info.rs");

fn main() {
    println!("cargo:rerun-if-changed=../shared/version_info.rs");
    // The GUI's dependencies bring a manifest of their own, so the one .cargo/config.toml
    // asks the linker for on this target would be a duplicate resource.
    if std::env::var("TARGET").as_deref() == Ok("aarch64-pc-windows-msvc") {
        println!("cargo:rustc-link-arg=/MANIFEST:NO");
    }
    embed_version_info(
        "AnyGenesis Builder",
        "anygenesis-gui.exe",
        Some("assets/icon.ico"),
    );
}
