include!("../shared/version_info.rs");

fn main() {
    println!("cargo:rerun-if-changed=../shared/version_info.rs");
    embed_version_info(
        "AnyGenesis Builder",
        "anygenesis-gui.exe",
        Some("assets/icon.ico"),
    );
}
