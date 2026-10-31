//! The per-user build cache: one folder per ROM SHA-1 (and code patch hash, M6.2), reused
//! while the ROM, anygenesis, the player library, the Zig version, the discovery stage and
//! the code patches all stay the same. Zig keeps its own cache in it too (`zig_folder`).

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anygenesis_rom::Sha1Digest;
use anyhow::{Context, Result, bail};

/// Overrides the cache folder (tests use their own).
const CACHE_VAR: &str = "ANYGENESIS_CACHE";

/// The file in an entry that holds its key, written once the build in it has finished.
const KEY_FILE: &str = "key";

/// The cache folder: `ANYGENESIS_CACHE`, else %LOCALAPPDATA%\AnyGenesis\cache on Windows and
/// $XDG_CACHE_HOME/anygenesis (else ~/.cache/anygenesis) elsewhere.
///
/// # Errors
/// None of the variables it is found from is set.
pub fn root() -> Result<PathBuf> {
    if let Some(folder) = env::var_os(CACHE_VAR) {
        return Ok(PathBuf::from(folder));
    }
    if cfg!(windows) {
        let local = env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
        return Ok(Path::new(&local).join("AnyGenesis").join("cache"));
    }
    if let Some(cache) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Ok(Path::new(&cache).join("anygenesis"));
    }
    let Some(home) = env::var_os("HOME") else {
        bail!("neither XDG_CACHE_HOME nor HOME is set, so there is no cache folder");
    };
    Ok(Path::new(&home).join(".cache").join("anygenesis"))
}

/// Zig's cache in the cache folder `root`, shared by every game: the C library, compiler-rt
/// and libunwind it builds on the first link.
pub fn zig_folder(root: &Path) -> PathBuf {
    root.join("zig")
}

/// One ROM's folder in the cache.
#[derive(Debug)]
pub struct Entry {
    pub folder: PathBuf,
    /// What a finished build in the folder must have been made from.
    key: String,
}

impl Entry {
    /// The entry for the ROM with SHA-1 `rom` in `root`, built by the anygenesis at `tool`
    /// with the player library at `player` and Zig `zig` (its version), discovering code up
    /// to the stage `discovery`, with the code patches whose hash is `patches`, if any. A
    /// patched build gets a folder of its own, so the plain build's stays.
    ///
    /// # Errors
    /// Reading `tool` or `player`.
    pub fn new(
        root: &Path,
        rom: Sha1Digest,
        tool: &Path,
        player: &Path,
        zig: &str,
        discovery: &str,
        patches: Option<Sha1Digest>,
    ) -> Result<Self> {
        let mut key = format!(
            "rom {rom}\nanygenesis {}\nplayer {}\nzig {zig}\ndiscovery {discovery}\n",
            file_sha1(tool)?,
            file_sha1(player)?
        );
        let folder = match patches {
            Some(patches) => {
                key.push_str(&format!("patches {patches}\n"));
                root.join(format!("{rom}-{patches}"))
            }
            None => root.join(rom.to_string()),
        };
        Ok(Self { folder, key })
    }

    /// Whether the folder holds a finished build from the same ROM, anygenesis, player, Zig
    /// and stage.
    pub fn is_current(&self) -> bool {
        fs::read_to_string(self.folder.join(KEY_FILE)).is_ok_and(|key| key == self.key)
    }

    /// Forgets the build in the folder, so an interrupted rebuild never counts as current.
    ///
    /// # Errors
    /// Removing the key file.
    pub fn invalidate(&self) -> Result<()> {
        let path = self.folder.join(KEY_FILE);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// Marks the build in the folder finished.
    ///
    /// # Errors
    /// Writing the key file.
    pub fn finish(&self) -> Result<()> {
        let path = self.folder.join(KEY_FILE);
        fs::write(&path, &self.key).with_context(|| format!("writing {}", path.display()))
    }
}

fn file_sha1(path: &Path) -> Result<Sha1Digest> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Sha1Digest::of(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_is_current_only_after_finish_and_until_a_tool_changes() {
        let root = env::temp_dir().join(format!("anygenesis-cache-test-{}", std::process::id()));
        fs::create_dir_all(&root).expect("the test folder is created");
        let (tool, player) = (root.join("tool"), root.join("player"));
        fs::write(&tool, b"tool 1").expect("written");
        fs::write(&player, b"player 1").expect("written");
        let rom = Sha1Digest::of(b"rom");
        let entry_for = |zig: &str, discovery: &str, patches: Option<Sha1Digest>| {
            Entry::new(&root, rom, &tool, &player, zig, discovery, patches).expect("key")
        };
        let entry = entry_for("0.17.0", "static", None);
        assert_eq!(entry.folder, root.join(rom.to_string()));
        fs::create_dir_all(&entry.folder).expect("the entry folder is created");
        assert!(!entry.is_current());
        entry.finish().expect("the key is written");
        assert!(entry.is_current());

        assert!(!entry_for("0.17.0", "profile", None).is_current());
        assert!(!entry_for("0.18.0", "static", None).is_current());
        let hash = Sha1Digest::of(b"patches");
        let patched = entry_for("0.17.0", "static", Some(hash));
        assert_eq!(patched.folder, root.join(format!("{rom}-{hash}")));
        fs::create_dir_all(&patched.folder).expect("the entry folder is created");
        assert!(!patched.is_current());
        patched.finish().expect("the key is written");
        assert!(patched.is_current() && entry.is_current());
        fs::write(&player, b"player 2").expect("written");
        assert!(!entry_for("0.17.0", "static", None).is_current());
        entry.invalidate().expect("the key is removed");
        assert!(!entry.is_current());
        entry.invalidate().expect("removing a missing key is fine");
        fs::remove_dir_all(&root).expect("the test folder is removed");
    }
}
