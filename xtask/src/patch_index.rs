//! Writes `index.toml`, the Marketplace's index, into a clone of meh7an/AnyGenesis-Patches:
//! every committed `<game>/<patch>/pack.toml` with its folder's committed files, their sizes
//! and SHA-256s, at the clone's HEAD. The bytes come from the commit, not the working tree,
//! because Git may change line endings on checkout and the app fetches the committed bytes.
//! Flow: commit the packs, run this on the clean clone, commit the index.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anygenesis_patch::{
    Pack,
    market::{INDEX_FILE, INDEX_FORMAT, Index, IndexFile, IndexPack, PACK_FILE},
    sha256,
};
use anyhow::{Context, Result, bail};

pub(crate) fn run(clone: Option<&str>) -> Result<ExitCode> {
    let Some(clone) = clone.map(PathBuf::from) else {
        bail!("usage: cargo xtask patch-index <AnyGenesis-Patches clone>");
    };
    let changed: Vec<String> = text(&git(&clone, &["status", "--porcelain"])?)?
        .lines()
        .filter(|line| {
            line.get(3..)
                .is_none_or(|path| path.trim_matches('"') != INDEX_FILE)
        })
        .map(str::to_owned)
        .collect();
    if !changed.is_empty() {
        bail!(
            "commit the packs first; the clone has changes:\n{}",
            changed.join("\n")
        );
    }
    let commit = text(&git(&clone, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in text(&git(&clone, &["ls-files", "-z"])?)?.split('\0') {
        let names: Vec<&str> = path.splitn(3, '/').collect();
        if let [game, patch, file] = names[..] {
            folders
                .entry(format!("{game}/{patch}"))
                .or_default()
                .push(file.to_owned());
        }
    }
    let mut packs = Vec::new();
    for (id, files) in &folders {
        if files.iter().any(|file| file == PACK_FILE) {
            packs.push(index_pack(&clone, &commit, id, files)?);
        }
    }
    let index = Index {
        format: INDEX_FORMAT,
        commit: commit.clone(),
        packs,
    };
    let text = format!(
        "# The Marketplace index, written by `cargo xtask patch-index` in meh7an/AnyGenesis.\n\
         # Do not edit; run it again after committing the packs.\n\n{}",
        index.to_toml()?
    );
    let path = clone.join(INDEX_FILE);
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    println!(
        "{}: {} packs at {commit}",
        path.display(),
        index.packs.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn index_pack(clone: &Path, commit: &str, id: &str, files: &[String]) -> Result<IndexPack> {
    let pack_path = files_path(clone, id).join(PACK_FILE);
    let pack = Pack::load(&pack_path).with_context(|| format!("pack {id}"))?;
    let files = files
        .iter()
        .map(|file| {
            let bytes = git(
                clone,
                &["cat-file", "blob", &format!("{commit}:{id}/{file}")],
            )?;
            Ok(IndexFile {
                path: file.clone(),
                size: bytes.len() as u64,
                sha256: sha256::hex_digest(&bytes),
            })
        })
        .collect::<Result<_>>()?;
    Ok(IndexPack {
        id: id.to_owned(),
        name: pack.name,
        description: pack.description,
        rom: pack.rom.to_string(),
        runs_code: !pack.code.sources.is_empty(),
        files,
    })
}

fn files_path(clone: &Path, id: &str) -> PathBuf {
    id.split('/')
        .fold(clone.to_path_buf(), |path, name| path.join(name))
}

/// The standard output of `git -C <clone> <args>`.
fn git(clone: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(args)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn text(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).context("git printed something that is not UTF-8")
}
