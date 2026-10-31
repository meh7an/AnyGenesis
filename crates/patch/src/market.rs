//! The Marketplace: the index of the packs in meh7an/AnyGenesis-Patches (`index.toml` at the
//! repository's root, written by `cargo xtask patch-index`) and installing a pack from it. A
//! pack's C code runs as native code in the game, so every name in an index is checked
//! before it becomes a path, and every file's size and SHA-256 before anything is written.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use anygenesis_rom::Sha1Digest;
use serde::{Deserialize, Serialize};

use crate::sha256;

/// The index format this build reads.
pub const INDEX_FORMAT: u32 = 1;
/// The index's name at the repository's root.
pub const INDEX_FILE: &str = "index.toml";
/// The file an install writes into the pack's folder, naming what it installed.
pub const INSTALLED_FILE: &str = "installed.toml";
/// The pack's own file, which every pack has.
pub const PACK_FILE: &str = "pack.toml";
/// The largest index and the largest file in a pack.
pub const MAX_FILE_SIZE: u64 = 1 << 20;
/// The most one pack's files may add up to.
pub const MAX_PACK_SIZE: u64 = 8 << 20;

/// The longest name of one folder or file.
const MAX_COMPONENT_LEN: usize = 64;
/// The longest pack name shown.
const MAX_NAME_LEN: usize = 64;
/// The longest description shown.
const MAX_DESCRIPTION_LEN: usize = 300;
/// Names Windows keeps for devices, with any extension.
const RESERVED_NAMES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// What can go wrong reading an index or installing a pack.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MarketError {
    #[error("the Marketplace index is not valid: {0}")]
    Index(String),
    #[error("fetching {path}: {reason}")]
    Fetch { path: String, reason: String },
    #[error("{path} is {actual} bytes, the index says {expected}")]
    Size {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("{path} does not match the checksum in the index")]
    Checksum { path: String },
    #[error("{} is not a folder the Marketplace installed", .0.display())]
    Foreign(PathBuf),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The Marketplace index: every pack at one commit of the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub format: u32,
    /// The commit every listed file is taken from.
    pub commit: String,
    #[serde(default, rename = "pack")]
    pub packs: Vec<IndexPack>,
}

/// One pack in the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexPack {
    /// The pack's folder in the repository, `/`-separated, also its folder once installed.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The SHA-1 of the unpatched ROM the pack is for.
    pub rom: String,
    /// Whether the pack carries C code, which runs as native code in the game.
    pub runs_code: bool,
    #[serde(rename = "file")]
    pub files: Vec<IndexFile>,
}

/// One file of a pack, its path relative to the pack's folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// What an install leaves in the pack's folder.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installed {
    commit: String,
    #[serde(rename = "file")]
    files: Vec<IndexFile>,
}

impl Index {
    /// The index in `text`, every name and checksum checked.
    ///
    /// # Errors
    /// [`MarketError::Index`] for text that does not parse or an index that fails a check.
    pub fn parse(text: &str) -> Result<Self, MarketError> {
        let index: Self =
            toml::from_str(text).map_err(|error| MarketError::Index(error.to_string()))?;
        index.check().map_err(MarketError::Index)?;
        Ok(index)
    }

    /// The index as `index.toml` text.
    ///
    /// # Errors
    /// [`MarketError::Index`] for an index that fails a check.
    pub fn to_toml(&self) -> Result<String, MarketError> {
        self.check().map_err(MarketError::Index)?;
        toml::to_string(self).map_err(|error| MarketError::Index(error.to_string()))
    }

    /// The packs for the ROM with SHA-1 `rom`, in index order.
    pub fn for_rom(&self, rom: Sha1Digest) -> Vec<&IndexPack> {
        self.packs
            .iter()
            .filter(|pack| {
                pack.rom
                    .parse::<Sha1Digest>()
                    .is_ok_and(|digest| digest == rom)
            })
            .collect()
    }

    fn check(&self) -> Result<(), String> {
        if self.format != INDEX_FORMAT {
            return Err(format!(
                "format {} (this build reads format {INDEX_FORMAT}); update AnyGenesis",
                self.format
            ));
        }
        if !is_hex(&self.commit, 40) {
            return Err(format!(
                "commit `{}` is not a full commit hash",
                self.commit
            ));
        }
        let mut ids = HashSet::new();
        for pack in &self.packs {
            pack.check()
                .map_err(|reason| format!("pack `{}`: {reason}", pack.id))?;
            if !ids.insert(pack.id.to_ascii_lowercase()) {
                return Err(format!("pack `{}` is listed twice", pack.id));
            }
        }
        Ok(())
    }
}

impl IndexPack {
    /// The repository path of `file`, `/`-separated.
    pub fn file_path(&self, file: &IndexFile) -> String {
        format!("{}/{}", self.id, file.path)
    }

    /// The total size of the pack's files.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// The pack's folder under the install root `root`.
    pub fn folder(&self, root: &Path) -> PathBuf {
        join(root, &self.id)
    }

    fn check(&self) -> Result<(), String> {
        check_path(&self.id)?;
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_NAME_LEN {
            return Err(format!(
                "the name is empty or over {MAX_NAME_LEN} characters"
            ));
        }
        if self
            .description
            .as_ref()
            .is_some_and(|description| description.chars().count() > MAX_DESCRIPTION_LEN)
        {
            return Err(format!(
                "the description is over {MAX_DESCRIPTION_LEN} characters"
            ));
        }
        let shown = [Some(&self.name), self.description.as_ref()];
        if shown
            .into_iter()
            .flatten()
            .any(|text| text.chars().any(char::is_control))
        {
            return Err("the name or description holds a control character".to_owned());
        }
        self.rom
            .parse::<Sha1Digest>()
            .map_err(|error| format!("rom: {error}"))?;
        let mut paths = HashSet::new();
        for file in &self.files {
            check_path(&file.path)?;
            if file.path.eq_ignore_ascii_case(INSTALLED_FILE) {
                return Err(format!("`{INSTALLED_FILE}` is the Marketplace's own name"));
            }
            if !paths.insert(file.path.to_ascii_lowercase()) {
                return Err(format!("`{}` is listed twice", file.path));
            }
            if file.size > MAX_FILE_SIZE {
                return Err(format!("`{}` is over {MAX_FILE_SIZE} bytes", file.path));
            }
            if !is_hex(&file.sha256, 64) {
                return Err(format!("`{}` has no valid SHA-256", file.path));
            }
        }
        if !paths.contains(PACK_FILE) {
            return Err(format!("it has no `{PACK_FILE}`"));
        }
        if self.size() > MAX_PACK_SIZE {
            return Err(format!("its files are over {MAX_PACK_SIZE} bytes"));
        }
        Ok(())
    }
}

/// Checks a `/`-separated relative path of one or two names: ASCII letters, digits, space,
/// `-`, `_` and `.` only, no name a parent, hidden, or a Windows device, and no name Windows
/// would change (a trailing dot or space).
fn check_path(path: &str) -> Result<(), String> {
    let names: Vec<&str> = path.split('/').collect();
    if names.len() > 2 {
        return Err(format!("`{path}` is more than two folders deep"));
    }
    for name in names {
        let allowed = |c: char| c.is_ascii_alphanumeric() || " -_.".contains(c);
        if name.is_empty()
            || name.len() > MAX_COMPONENT_LEN
            || !name.chars().all(allowed)
            || name.starts_with(['.', ' '])
            || name.ends_with(['.', ' '])
            || is_device(name)
        {
            return Err(format!("`{path}` is not an allowed path"));
        }
    }
    Ok(())
}

/// Whether the ASCII `name` is a Windows device name (`CON`, `COM1`, ...) with any extension,
/// in any case.
fn is_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let numbered = stem.len() == 4
        && ["COM", "LPT"]
            .iter()
            .any(|prefix| stem[..3].eq_ignore_ascii_case(prefix))
        && stem.as_bytes()[3].is_ascii_digit();
    numbered
        || RESERVED_NAMES
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `path`, already checked, as a path under `root`.
fn join(root: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(root.to_path_buf(), |folder, name| folder.join(name))
}

/// Where the index and the packs' files come from.
pub trait Source {
    /// The index's text, at most [`MAX_FILE_SIZE`] bytes.
    ///
    /// # Errors
    /// Why it could not be fetched, shown to the user.
    fn index(&self) -> Result<Vec<u8>, String>;

    /// The repository file `path` (`/`-separated) as of `commit`, at most `limit` bytes; a
    /// longer one may be cut at `limit + 1` bytes.
    ///
    /// # Errors
    /// Why it could not be fetched, shown to the user.
    fn file(&self, commit: &str, path: &str, limit: u64) -> Result<Vec<u8>, String>;
}

/// A local clone or copy of the repository, for tests and development; the commit is not
/// looked at.
#[derive(Debug, Clone)]
pub struct LocalSource {
    pub root: PathBuf,
}

impl LocalSource {
    fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        let path = join(&self.root, path);
        let size = fs::metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .len();
        if size > limit {
            return Err(format!("{} is over {limit} bytes", path.display()));
        }
        fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

impl Source for LocalSource {
    fn index(&self) -> Result<Vec<u8>, String> {
        self.read(INDEX_FILE, MAX_FILE_SIZE)
    }

    fn file(&self, _commit: &str, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        self.read(path, limit)
    }
}

/// The index from `source`.
///
/// # Errors
/// [`MarketError::Fetch`] when it cannot be fetched, [`MarketError::Index`] when it is not
/// valid.
pub fn fetch_index(source: &dyn Source) -> Result<Index, MarketError> {
    let bytes = source.index().map_err(|reason| MarketError::Fetch {
        path: INDEX_FILE.to_owned(),
        reason,
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| MarketError::Index("the index is not UTF-8 text".to_owned()))?;
    Index::parse(&text)
}

/// A pack's state under the install root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Absent,
    Installed {
        /// The index lists other files than the ones installed.
        update: bool,
        /// An installed file was changed or removed on this computer.
        modified: bool,
    },
}

/// The state of `pack` under the install root `root`.
pub fn status(root: &Path, pack: &IndexPack) -> Status {
    let folder = pack.folder(root);
    let Some(installed) = read_installed(&folder) else {
        return Status::Absent;
    };
    let modified = installed.files.iter().any(|file| {
        fs::read(join(&folder, &file.path))
            .map_or(true, |bytes| sha256::hex_digest(&bytes) != file.sha256)
    });
    let listed = |files: &[IndexFile]| -> HashSet<(String, String)> {
        files
            .iter()
            .map(|file| (file.path.clone(), file.sha256.clone()))
            .collect()
    };
    Status::Installed {
        update: listed(&installed.files) != listed(&pack.files),
        modified,
    }
}

fn read_installed(folder: &Path) -> Option<Installed> {
    let text = fs::read_to_string(folder.join(INSTALLED_FILE)).ok()?;
    toml::from_str(&text).ok()
}

/// Installs or updates `pack` from `index` into its folder under `root`. Every file is
/// fetched and checked before anything is written; the pack's folder is replaced in one
/// rename, so a failed install leaves the one before it.
///
/// # Errors
/// [`MarketError::Fetch`], [`MarketError::Size`] and [`MarketError::Checksum`] for a file
/// that cannot be fetched or does not match the index, [`MarketError::Foreign`] when the
/// pack's folder exists without the Marketplace's [`INSTALLED_FILE`], [`MarketError::Io`]
/// when writing fails.
pub fn install(
    root: &Path,
    index: &Index,
    pack: &IndexPack,
    source: &dyn Source,
) -> Result<PathBuf, MarketError> {
    let folder = pack.folder(root);
    if folder.exists() && read_installed(&folder).is_none() {
        return Err(MarketError::Foreign(folder));
    }
    let contents = pack
        .files
        .iter()
        .map(|file| fetch_checked(index, pack, file, source))
        .collect::<Result<Vec<_>, _>>()?;
    let staging = root.join(format!(".staging-{}", std::process::id()));
    let result = stage(&staging, index, pack, &contents).and_then(|()| {
        let old = root.join(format!(".old-{}", std::process::id()));
        replace(&staging, &folder, &old)
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result.map(|()| folder)
}

fn fetch_checked(
    index: &Index,
    pack: &IndexPack,
    file: &IndexFile,
    source: &dyn Source,
) -> Result<Vec<u8>, MarketError> {
    let path = pack.file_path(file);
    let bytes = source
        .file(&index.commit, &path, file.size)
        .map_err(|reason| MarketError::Fetch {
            path: path.clone(),
            reason,
        })?;
    let actual = bytes.len() as u64;
    if actual != file.size {
        return Err(MarketError::Size {
            path,
            expected: file.size,
            actual,
        });
    }
    if sha256::hex_digest(&bytes) != file.sha256 {
        return Err(MarketError::Checksum { path });
    }
    Ok(bytes)
}

fn stage(
    staging: &Path,
    index: &Index,
    pack: &IndexPack,
    contents: &[Vec<u8>],
) -> Result<(), MarketError> {
    let _ = fs::remove_dir_all(staging);
    for (file, bytes) in pack.files.iter().zip(contents) {
        let path = join(staging, &file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        fs::write(&path, bytes).map_err(|source| io_error(&path, source))?;
    }
    let installed = Installed {
        commit: index.commit.clone(),
        files: pack.files.clone(),
    };
    let text =
        toml::to_string(&installed).map_err(|error| MarketError::Index(error.to_string()))?;
    let path = staging.join(INSTALLED_FILE);
    fs::write(&path, text).map_err(|source| io_error(&path, source))
}

/// Moves `staging` to `folder`, an existing `folder` out of the way to `old` first and back
/// if the move fails.
fn replace(staging: &Path, folder: &Path, old: &Path) -> Result<(), MarketError> {
    if let Some(parent) = folder.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    if !folder.exists() {
        return fs::rename(staging, folder).map_err(|source| io_error(folder, source));
    }
    let _ = fs::remove_dir_all(old);
    fs::rename(folder, old).map_err(|source| io_error(folder, source))?;
    if let Err(source) = fs::rename(staging, folder) {
        let _ = fs::rename(old, folder);
        return Err(io_error(folder, source));
    }
    fs::remove_dir_all(old).map_err(|source| io_error(old, source))
}

/// Removes the installed `pack` from under `root`, and its game's folder when that is left
/// empty.
///
/// # Errors
/// [`MarketError::Foreign`] when the folder holds no [`INSTALLED_FILE`], [`MarketError::Io`]
/// when removing fails.
pub fn remove(root: &Path, pack: &IndexPack) -> Result<(), MarketError> {
    let folder = pack.folder(root);
    if read_installed(&folder).is_none() {
        return Err(MarketError::Foreign(folder));
    }
    fs::remove_dir_all(&folder).map_err(|source| io_error(&folder, source))?;
    if let Some(parent) = folder.parent().filter(|parent| *parent != root) {
        // Only succeeds when the game's folder is empty, which is the only case wanted.
        let _ = fs::remove_dir(parent);
    }
    Ok(())
}

fn io_error(path: &Path, source: io::Error) -> MarketError {
    MarketError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const ROM: &str = "6ddb7de1e17e7f6cdb88927bd906352030daa194";
    const PACK_TEXT: &[u8] = b"name = \"Test\"\n";
    const CODE_TEXT: &[u8] = b"/* a hook */\n";

    /// A temp folder removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let folder = env::temp_dir().join(format!(
                "anygenesis-market-test-{}-{name}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(&folder).expect("the test folder is created");
            Self(folder)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn file(path: &str, bytes: &[u8]) -> IndexFile {
        IndexFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: sha256::hex_digest(bytes),
        }
    }

    fn pack(id: &str) -> IndexPack {
        IndexPack {
            id: id.to_owned(),
            name: "Widescreen".to_owned(),
            description: Some("The game at 400 pixels.".to_owned()),
            rom: ROM.to_owned(),
            runs_code: true,
            files: vec![file(PACK_FILE, PACK_TEXT), file("hook.c", CODE_TEXT)],
        }
    }

    fn index(packs: Vec<IndexPack>) -> Index {
        Index {
            format: INDEX_FORMAT,
            commit: COMMIT.to_owned(),
            packs,
        }
    }

    /// A repository copy holding `packs`' files with the test contents, and its index.
    fn repository(folder: &Path, packs: &[IndexPack]) -> LocalSource {
        for pack in packs {
            let pack_folder = pack.folder(folder);
            fs::create_dir_all(&pack_folder).expect("the pack folder is created");
            fs::write(pack_folder.join(PACK_FILE), PACK_TEXT).expect("pack.toml is written");
            fs::write(pack_folder.join("hook.c"), CODE_TEXT).expect("hook.c is written");
        }
        let text = index(packs.to_vec())
            .to_toml()
            .expect("the index is written");
        fs::write(folder.join(INDEX_FILE), text).expect("index.toml is written");
        LocalSource {
            root: folder.to_owned(),
        }
    }

    fn reason(index: &Index) -> String {
        match index.check() {
            Err(reason) => reason,
            Ok(()) => panic!("expected the index to be refused"),
        }
    }

    #[test]
    fn test_index_round_trips_through_toml() {
        let index = index(vec![pack("Sonic the Hedgehog/widescreen")]);
        let text = index.to_toml().expect("the index is written");
        assert_eq!(Index::parse(&text).expect("the index parses"), index);
    }

    #[test]
    fn test_index_unknown_format_refused() {
        let mut index = index(Vec::new());
        index.format = 2;
        assert!(reason(&index).contains("format 2"));
    }

    #[test]
    fn test_index_unknown_field_refused() {
        let text = format!("format = 1\ncommit = \"{COMMIT}\"\nmirror = \"https://x\"\n");
        assert!(matches!(Index::parse(&text), Err(MarketError::Index(_))));
    }

    #[test]
    fn test_index_short_commit_refused() {
        let mut index = index(Vec::new());
        index.commit = "f2af94a".to_owned();
        assert!(reason(&index).contains("commit"));
    }

    #[test]
    fn test_index_unsafe_paths_refused() {
        for path in [
            "", ".", "..", "../x", "x/..", "x/../y", "/x", "x/", "C:x", "x\\y", "x/y/z", ".git",
            " x", "x ", "x.", "CON", "con.txt", "Com1", "lpt9.c", "x:y", "x*", "\u{e9}",
        ] {
            let mut bad_id = pack(path);
            bad_id.files.truncate(1);
            assert!(index(vec![bad_id]).check().is_err(), "id `{path}` accepted");
            let mut bad_file = pack("Game/patch");
            bad_file.files.push(file(path, b""));
            assert!(
                index(vec![bad_file]).check().is_err(),
                "file `{path}` accepted"
            );
        }
        let mut good = pack("Sonic the Hedgehog/widescreen REV01");
        good.files.push(file("art/tiles.bin", b"x"));
        assert!(index(vec![good]).check().is_ok());
    }

    #[test]
    fn test_index_pack_faults_refused() {
        let mut no_pack_file = pack("Game/patch");
        no_pack_file.files.remove(0);
        assert!(reason(&index(vec![no_pack_file])).contains(PACK_FILE));
        let mut own_name = pack("Game/patch");
        own_name.files.push(file(INSTALLED_FILE, b""));
        assert!(reason(&index(vec![own_name])).contains(INSTALLED_FILE));
        let mut twice = pack("Game/patch");
        twice.files.push(file("HOOK.C", CODE_TEXT));
        assert!(reason(&index(vec![twice])).contains("twice"));
        let mut large = pack("Game/patch");
        large.files[1].size = MAX_FILE_SIZE + 1;
        assert!(reason(&index(vec![large])).contains("bytes"));
        let mut bad_sha = pack("Game/patch");
        bad_sha.files[1].sha256 = bad_sha.files[1].sha256.to_ascii_uppercase();
        assert!(reason(&index(vec![bad_sha])).contains("SHA-256"));
        let mut bad_rom = pack("Game/patch");
        bad_rom.rom = "xyz".to_owned();
        assert!(reason(&index(vec![bad_rom])).contains("rom"));
        let mut control = pack("Game/patch");
        control.name = "Wide\u{1b}[2J".to_owned();
        assert!(reason(&index(vec![control])).contains("control"));
        assert!(reason(&index(vec![pack("Game/patch"), pack("game/PATCH")])).contains("twice"));
    }

    #[test]
    fn test_index_for_rom_lists_only_that_rom() {
        let mut other = pack("Other/patch");
        other.rom = "8bca5dcef1af3e00098666fd892dc1c2a76333f9".to_owned();
        let index = index(vec![pack("Game/patch"), other]);
        let rom = ROM.parse().expect("a SHA-1");
        let ids: Vec<&str> = index
            .for_rom(rom)
            .iter()
            .map(|pack| pack.id.as_str())
            .collect();
        assert_eq!(ids, ["Game/patch"]);
    }

    #[test]
    fn test_install_writes_the_pack_and_reports_it() {
        let (repo, root) = (Folder::new("install-repo"), Folder::new("install-root"));
        let pack = pack("Sonic the Hedgehog/widescreen");
        let source = repository(&repo.0, std::slice::from_ref(&pack));
        let index = fetch_index(&source).expect("the index is fetched");
        assert_eq!(status(&root.0, &pack), Status::Absent);
        let folder = install(&root.0, &index, &pack, &source).expect("the pack installs");
        assert_eq!(folder, root.0.join("Sonic the Hedgehog").join("widescreen"));
        assert_eq!(fs::read(folder.join("hook.c")).expect("hook.c"), CODE_TEXT);
        let installed = Status::Installed {
            update: false,
            modified: false,
        };
        assert_eq!(status(&root.0, &pack), installed);
        let leftovers = fs::read_dir(&root.0).expect("the root").count();
        assert_eq!(leftovers, 1, "only the game's folder is left");
    }

    #[test]
    fn test_install_status_sees_updates_and_local_edits() {
        let (repo, root) = (Folder::new("status-repo"), Folder::new("status-root"));
        let pack = pack("Game/patch");
        let source = repository(&repo.0, std::slice::from_ref(&pack));
        let index = fetch_index(&source).expect("the index is fetched");
        let folder = install(&root.0, &index, &pack, &source).expect("the pack installs");
        let mut newer = pack.clone();
        newer.files[1] = file("hook.c", b"/* a newer hook */\n");
        assert_eq!(
            status(&root.0, &newer),
            Status::Installed {
                update: true,
                modified: false
            }
        );
        fs::write(folder.join("hook.c"), b"edited").expect("hook.c is edited");
        assert_eq!(
            status(&root.0, &pack),
            Status::Installed {
                update: false,
                modified: true
            }
        );
        let hook = repo.0.join("Game").join("patch").join("hook.c");
        fs::write(hook, b"/* a newer hook */\n").expect("the repository is updated");
        install(&root.0, &index, &newer, &source).expect("the update installs");
        assert_eq!(
            fs::read(folder.join("hook.c")).expect("hook.c"),
            b"/* a newer hook */\n"
        );
    }

    #[test]
    fn test_install_checksum_mismatch_writes_nothing() {
        let (repo, root) = (Folder::new("checksum-repo"), Folder::new("checksum-root"));
        let pack = pack("Game/patch");
        let source = repository(&repo.0, std::slice::from_ref(&pack));
        let index = fetch_index(&source).expect("the index is fetched");
        let hook = repo.0.join("Game").join("patch").join("hook.c");
        fs::write(&hook, b"/* a hooK */\n").expect("tampered");
        let error = install(&root.0, &index, &pack, &source).expect_err("refused");
        assert!(matches!(error, MarketError::Checksum { .. }), "{error}");
        fs::write(&hook, b"/* hook */\n").expect("tampered");
        let error = install(&root.0, &index, &pack, &source).expect_err("refused");
        assert!(matches!(error, MarketError::Size { .. }), "{error}");
        fs::write(&hook, b"/* a longer hook */\n").expect("tampered");
        let error = install(&root.0, &index, &pack, &source).expect_err("refused");
        assert!(matches!(error, MarketError::Fetch { .. }), "{error}");
        assert_eq!(fs::read_dir(&root.0).expect("the root").count(), 0);
    }

    #[test]
    fn test_install_failed_update_keeps_the_installed_pack() {
        let (repo, root) = (Folder::new("keep-repo"), Folder::new("keep-root"));
        let pack = pack("Game/patch");
        let source = repository(&repo.0, std::slice::from_ref(&pack));
        let index = fetch_index(&source).expect("the index is fetched");
        let folder = install(&root.0, &index, &pack, &source).expect("the pack installs");
        let mut newer = pack.clone();
        newer.files[1] = file("hook.c", b"/* never fetched */\n");
        assert!(install(&root.0, &index, &newer, &source).is_err());
        assert_eq!(fs::read(folder.join("hook.c")).expect("hook.c"), CODE_TEXT);
    }

    #[test]
    fn test_install_and_remove_leave_foreign_folders_alone() {
        let (repo, root) = (Folder::new("foreign-repo"), Folder::new("foreign-root"));
        let pack = pack("Game/patch");
        let source = repository(&repo.0, std::slice::from_ref(&pack));
        let index = fetch_index(&source).expect("the index is fetched");
        let folder = pack.folder(&root.0);
        fs::create_dir_all(&folder).expect("a folder of the user's own");
        fs::write(folder.join("notes.txt"), b"mine").expect("a file of the user's own");
        let error = install(&root.0, &index, &pack, &source).expect_err("refused");
        assert!(matches!(error, MarketError::Foreign(_)), "{error}");
        assert!(matches!(
            remove(&root.0, &pack),
            Err(MarketError::Foreign(_))
        ));
        assert!(folder.join("notes.txt").exists());
    }

    #[test]
    fn test_remove_deletes_the_pack_and_its_empty_game_folder() {
        let (repo, root) = (Folder::new("remove-repo"), Folder::new("remove-root"));
        let pack = pack("Game/patch");
        let source = repository(&repo.0, std::slice::from_ref(&pack));
        let index = fetch_index(&source).expect("the index is fetched");
        install(&root.0, &index, &pack, &source).expect("the pack installs");
        remove(&root.0, &pack).expect("the pack is removed");
        assert_eq!(status(&root.0, &pack), Status::Absent);
        assert!(!root.0.join("Game").exists());
    }
}
