//! THIRD-PARTY-NOTICES.txt for a release: every Rust crate compiled into anygenesis,
//! anygenesis-gui and the player library (from `cargo tree` over Cargo.lock, for this host's
//! targets) with the licence texts each crate ships, and the non-crate parts of a download (Zig
//! and what it carries, the GUI's fonts and icons). Fails on a crate whose licence is not on
//! the allow list, a crate with no text to show, or a test-only crate in the tree.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::toolchain::HOST;

/// SPDX identifiers a shipped crate may be under (one of them, for an OR choice).
const ALLOWED: [&str; 15] = [
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "MIT",
    "MIT-0",
    "MPL-2.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Unlicense",
    "Zlib",
];

/// `<license> WITH <exception>` pairs allowed on top of `ALLOWED`.
const ALLOWED_EXCEPTIONS: [(&str, &str); 1] = [("Apache-2.0", "LLVM-exception")];

/// Crates that only test against third-party code (Musashi, ymfm, SingleStepTests) and must
/// never reach a release.
const TEST_ONLY: [&str; 2] = ["anygenesis-oracle", "anygenesis-c-oracle"];

/// Build dependencies whose sources are compiled into the programs: SDL, built from sdl3-src
/// into the player library and every game.
const SOURCE_CRATES: [&str; 1] = ["sdl3-src"];

/// Licence texts a crate keeps outside its root folder: SDL's own, inside sdl3-src.
const EXTRA_TEXTS: [(&str, &str); 1] = [("sdl3-src", "SDL/LICENSE.txt")];

/// One crate from `cargo tree`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Crate {
    name: String,
    version: String,
    license: String,
    repository: String,
}

/// The notices text for the release folder `stage` (its toolchain/zig holds Zig's texts).
///
/// # Errors
/// `cargo tree` fails, a licence is not allowed, a text cannot be read or a test-only crate
/// is in the tree.
pub(crate) fn text(root: &Path, stage: &Path) -> Result<String> {
    let tools = ["anygenesis", "anygenesis-gui"];
    let mut crates = cargo_tree(root, &tools, HOST.tool_target, "normal")?;
    let player = ["anygenesis-player-lib"];
    crates.extend(cargo_tree(root, &player, HOST.player_target, "normal")?);
    let built = cargo_tree(root, &player, HOST.player_target, "normal,build")?;
    for name in SOURCE_CRATES {
        let before = crates.len();
        crates.extend(built.iter().filter(|krate| krate.name == name).cloned());
        if crates.len() == before {
            bail!("{name} is no longer in the player library's build: update SOURCE_CRATES");
        }
    }
    crates.sort();
    crates.dedup();

    let zig = stage.join("toolchain").join("zig");
    let assets = root.join("crates").join("gui").join("assets");
    let components = [
        ("Zig 0.17.0 (toolchain/zig)", zig.join("LICENSE")),
        (
            "LLVM, Clang, LLD, libunwind and compiler-rt, inside Zig (toolchain/zig)",
            zig.join("lib").join("libunwind").join("LICENSE.TXT"),
        ),
        (
            "The C library sources and headers Zig builds games with (toolchain/zig/lib/libc)",
            zig.join(HOST.libc_notice),
        ),
        (
            "IBM Plex Sans and IBM Plex Mono fonts, inside anygenesis-gui",
            assets.join("fonts").join("LICENSE.txt"),
        ),
        (
            "Phosphor icons, inside anygenesis-gui",
            assets.join("icons").join("LICENSE"),
        ),
    ];

    let mut out = format!(
        "AnyGenesis {} third-party notices\n\n\
         AnyGenesis itself is Copyright (c) 2026 Mehran, under MIT OR Apache-2.0 (LICENSE-MIT,\n\
         LICENSE-APACHE). This file lists every third-party part of this download and of the\n\
         games it builds, with its licence texts: first the non-Rust parts, then the Rust\n\
         crates, grouped by identical text.\n",
        env!("CARGO_PKG_VERSION")
    );
    for (title, path) in components {
        push_block(&mut out, &[title.to_owned()], &read(&path)?);
    }

    let registry = registry_folders()?;
    let mut groups: BTreeMap<String, (Vec<String>, String)> = BTreeMap::new();
    for krate in &crates {
        if !allowed(&krate.license) {
            bail!(
                "{} {} is under `{}`, which is not on the allow list",
                krate.name,
                krate.version,
                krate.license
            );
        }
        let label = format!("{} {} ({})", krate.name, krate.version, krate.license);
        for text in crate_texts(krate, &registry, root)? {
            let key = text.split_whitespace().collect::<Vec<_>>().join(" ");
            groups
                .entry(key)
                .or_insert_with(|| (Vec::new(), text))
                .0
                .push(label.clone());
        }
    }
    let mut blocks: Vec<_> = groups.into_values().collect();
    blocks.sort();
    for (labels, text) in blocks {
        push_block(&mut out, &labels, &text);
    }
    Ok(out)
}

/// The registry crates `cargo tree` lists for `packages` on `target` along the dependency
/// `edges` ("normal" is what is compiled into the programs; build tools are not shipped).
fn cargo_tree(root: &Path, packages: &[&str], target: &str, edges: &str) -> Result<Vec<Crate>> {
    let output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root)
        .args(["tree", "--locked", "-e", edges, "--prefix", "none"])
        .args(["--format", "{p}\t{l}\t{r}", "--target", target])
        .args(packages.iter().flat_map(|package| ["-p", package]))
        .output()
        .context("running cargo tree")?;
    if !output.status.success() {
        bail!(
            "cargo tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut crates = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim_end_matches(" (*)");
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let package = fields.next().unwrap_or_default();
        let (name, rest) = package
            .split_once(" v")
            .with_context(|| format!("cargo tree printed `{line}`"))?;
        if TEST_ONLY.contains(&name) {
            bail!("the test-only crate {name} is in the release tree");
        }
        let (version, source) = rest.split_once(' ').unwrap_or((rest, ""));
        // A path source in brackets is one of ours; "(proc-macro)" marks a registry macro.
        if source.starts_with('(') && source != "(proc-macro)" {
            continue;
        }
        crates.push(Crate {
            name: name.to_owned(),
            version: version.to_owned(),
            license: fields.next().unwrap_or_default().to_owned(),
            repository: fields.next().unwrap_or_default().to_owned(),
        });
    }
    Ok(crates)
}

/// The folders cargo unpacks registry crates into.
fn registry_folders() -> Result<Vec<PathBuf>> {
    let home = match env::var_os("CARGO_HOME") {
        Some(home) => PathBuf::from(home),
        None => env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|home| PathBuf::from(home).join(".cargo"))
            .context("neither CARGO_HOME nor a home folder is set")?,
    };
    let src = home.join("registry").join("src");
    let mut folders = Vec::new();
    for entry in fs::read_dir(&src).with_context(|| format!("reading {}", src.display()))? {
        folders.push(
            entry
                .with_context(|| format!("reading {}", src.display()))?
                .path(),
        );
    }
    Ok(folders)
}

/// The licence texts `krate` ships in its root folder (and `EXTRA_TEXTS`), or, for a crate
/// that ships none, the standard text of the licence it offers.
fn crate_texts(krate: &Crate, registry: &[PathBuf], root: &Path) -> Result<Vec<String>> {
    let folder_name = format!("{}-{}", krate.name, krate.version);
    let folder = registry
        .iter()
        .map(|index| index.join(&folder_name))
        .find(|folder| folder.is_dir())
        .with_context(|| format!("{folder_name} is not unpacked in the cargo registry"))?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(&folder).with_context(|| format!("reading {}", folder.display()))? {
        let path = entry
            .with_context(|| format!("reading {}", folder.display()))?
            .path();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_uppercase())
            .unwrap_or_default();
        let is_text = [
            "LICENSE",
            "LICENCE",
            "COPYING",
            "UNLICENSE",
            "NOTICE",
            "COPYRIGHT",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix));
        if is_text && path.is_file() {
            paths.push(path);
        }
    }
    for (name, extra) in EXTRA_TEXTS {
        if name == krate.name {
            paths.push(folder.join(extra));
        }
    }
    paths.sort();
    if !paths.is_empty() {
        return paths.iter().map(|path| read(path)).collect();
    }
    let choices = alternatives(&krate.license);
    if choices.contains(&"Apache-2.0") {
        return Ok(vec![read(&root.join("LICENSE-APACHE"))?]);
    }
    if choices.contains(&"MIT") {
        let ours = read(&root.join("LICENSE-MIT"))?;
        let holder = format!(
            "Copyright (c) the {} authors ({})",
            krate.name, krate.repository
        );
        return Ok(vec![ours.replace("Copyright (c) 2026 Mehran", &holder)]);
    }
    bail!(
        "{folder_name} ships no licence text and offers neither Apache-2.0 nor MIT ({})",
        krate.license
    )
}

/// The licences of a plain choice (`A OR B`, or the old `A/B`); empty for anything else.
fn alternatives(expression: &str) -> Vec<&str> {
    let choices: Vec<&str> = expression
        .split(" OR ")
        .flat_map(|part| part.split('/'))
        .map(str::trim)
        .collect();
    if choices
        .iter()
        .any(|choice| choice.contains([' ', '(', ')']) || choice.is_empty())
    {
        return Vec::new();
    }
    choices
}

/// Whether the SPDX `expression` can be met with `ALLOWED` licences (`/` read as OR).
fn allowed(expression: &str) -> bool {
    let spaced = expression
        .replace('/', " OR ")
        .replace('(', " ( ")
        .replace(')', " ) ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut position = 0;
    or_expression(&tokens, &mut position) == Some(true) && position == tokens.len()
}

fn or_expression(tokens: &[&str], position: &mut usize) -> Option<bool> {
    let mut met = and_expression(tokens, position)?;
    while tokens.get(*position) == Some(&"OR") {
        *position += 1;
        let right = and_expression(tokens, position)?;
        met = met || right;
    }
    Some(met)
}

fn and_expression(tokens: &[&str], position: &mut usize) -> Option<bool> {
    let mut met = term(tokens, position)?;
    while tokens.get(*position) == Some(&"AND") {
        *position += 1;
        let right = term(tokens, position)?;
        met = met && right;
    }
    Some(met)
}

fn term(tokens: &[&str], position: &mut usize) -> Option<bool> {
    let token = *tokens.get(*position)?;
    *position += 1;
    if token == "(" {
        let met = or_expression(tokens, position)?;
        if tokens.get(*position) != Some(&")") {
            return None;
        }
        *position += 1;
        return Some(met);
    }
    if tokens.get(*position) == Some(&"WITH") {
        let exception = *tokens.get(*position + 1)?;
        *position += 2;
        return Some(ALLOWED_EXCEPTIONS.contains(&(token, exception)));
    }
    Some(ALLOWED.contains(&token))
}

fn push_block(out: &mut String, labels: &[String], text: &str) {
    out.push_str(&format!("\n{}\n", "-".repeat(80)));
    for label in labels {
        out.push_str(label);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(text.trim_end());
    out.push('\n');
}

/// A text file with its line endings made LF.
fn read(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_choices_and_combinations_are_met() {
        assert!(allowed("MIT OR Apache-2.0"));
        assert!(allowed("MIT/Apache-2.0"));
        assert!(allowed("(MIT OR Apache-2.0) AND Unicode-3.0"));
        assert!(allowed(
            "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"
        ));
        assert!(allowed("Apache-2.0 OR GPL-2.0-only"));
    }

    #[test]
    fn test_allowed_refuses_copyleft_unknown_and_malformed() {
        assert!(!allowed("GPL-3.0-only"));
        assert!(!allowed("MIT AND GPL-2.0-only"));
        assert!(!allowed("GPL-2.0-only WITH LLVM-exception"));
        assert!(!allowed(""));
        assert!(!allowed("(MIT"));
        assert!(!allowed("MIT Apache-2.0"));
    }

    #[test]
    fn test_alternatives_only_for_plain_choices() {
        assert_eq!(alternatives("MIT OR Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert_eq!(alternatives("MIT/Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert!(alternatives("(MIT OR Apache-2.0) AND Unicode-3.0").is_empty());
    }
}
