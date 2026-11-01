// The Windows version resource (file properties' Details tab) of a shipped program, shared by
// the build scripts of anygenesis and anygenesis-gui through `include!`.

/// Compiles a VERSIONINFO resource from the crate's version, and the program icon `icon` (a
/// path relative to the crate) when given, into the program being built. Does nothing for
/// other targets.
fn embed_version_info(description: &str, file_name: &str, icon: Option<&str>) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Cargo sets every variable read here for each build script, and a build script that
    // cannot write OUT_DIR or compile the resource has nothing better to do than stop.
    let number = |part: &str| std::env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap();
    let numbers = format!("{},{},{},0", number("MAJOR"), number("MINOR"), number("PATCH"));
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let icon_line = icon.map_or_else(String::new, |icon| {
        println!("cargo:rerun-if-changed={icon}");
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let path = std::path::Path::new(&manifest).join(icon);
        // rc reads backslashes in a string as escapes; forward slashes work as separators.
        format!("1 ICON \"{}\"\n", path.display().to_string().replace('\\', "/"))
    });
    let text = format!(
        "{icon_line}1 VERSIONINFO\n\
         FILEVERSION {numbers}\n\
         PRODUCTVERSION {numbers}\n\
         FILEOS 0x40004\n\
         FILETYPE 0x1\n\
         BEGIN\n\
         BLOCK \"StringFileInfo\"\n\
         BEGIN\n\
         BLOCK \"040904B0\"\n\
         BEGIN\n\
         VALUE \"FileDescription\", \"{description}\"\n\
         VALUE \"FileVersion\", \"{version}\"\n\
         VALUE \"LegalCopyright\", \"Copyright (c) 2026 Mehran, MIT OR Apache-2.0\"\n\
         VALUE \"OriginalFilename\", \"{file_name}\"\n\
         VALUE \"ProductName\", \"AnyGenesis\"\n\
         VALUE \"ProductVersion\", \"{version}\"\n\
         END\n\
         END\n\
         BLOCK \"VarFileInfo\"\n\
         BEGIN\n\
         VALUE \"Translation\", 0x409, 1200\n\
         END\n\
         END\n"
    );
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("version.rc");
    std::fs::write(&path, text).unwrap();
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
