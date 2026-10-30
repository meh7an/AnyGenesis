//! ROM-level patches, applied to a ROM image before analysis, so a patched image builds like
//! any other ROM: our own IPS, UPS and BPS appliers, inline byte runs, Game Genie codes, and
//! patch packs that list them for one ROM, with the code patches they carry (M6.2). The
//! appliers and the Game Genie decoder follow the public format specifications recorded in
//! plan/PROGRESS.md (M6.1).

mod bps;
mod code;
mod crc32;
mod genie;
mod ips;
mod pack;
mod reader;
mod ups;

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anygenesis_rom::{MAX_ROM_SIZE, Rom, RomError, Sha1Digest};

pub use code::{
    CodePatch, CodePatches, FlagValue, Flags, MAX_WIDESCREEN, RAM_START, RamPatch, Source, When,
};
pub use genie::GenieCode;
pub use pack::Pack;

/// The patch file formats with an applier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    Ips,
    Ups,
    Bps,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ips => "IPS",
            Self::Ups => "UPS",
            Self::Bps => "BPS",
        })
    }
}

/// Why a patch could not be loaded or applied. Every mismatch refuses the build: a patch
/// never applies to an image it was not made for.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatchError {
    #[error("reading {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is neither an IPS, UPS or BPS patch nor a .toml patch pack")]
    UnknownFormat { path: PathBuf },
    #[error("the {format} patch ends early")]
    Truncated { format: Format },
    #[error("the {format} patch is malformed: {reason}")]
    Malformed {
        format: Format,
        reason: &'static str,
    },
    #[error("the {format} patch is for a {expected}-byte image, not this {found}-byte one")]
    SourceSize {
        format: Format,
        expected: u64,
        found: usize,
    },
    #[error(
        "the {format} patch is for another image: its source CRC-32 is {expected:08x}, this \
         image's is {found:08x}"
    )]
    SourceCrc {
        format: Format,
        expected: u32,
        found: u32,
    },
    #[error("the image is already patched: it matches the {format} patch's target")]
    AlreadyPatched { format: Format },
    #[error(
        "the {format} patch made an image with CRC-32 {found:08x}, not the {expected:08x} it \
         records"
    )]
    TargetCrc {
        format: Format,
        expected: u32,
        found: u32,
    },
    #[error(
        "the {format} patch is damaged: its CRC-32 is {found:08x}, not the {expected:08x} it \
         records"
    )]
    PatchCrc {
        format: Format,
        expected: u32,
        found: u32,
    },
    #[error("the patched image would be {size} bytes, over the {MAX_ROM_SIZE}-byte limit")]
    TooLarge { size: u64 },
    #[error("{len} bytes written at {offset:#x} fall outside the {size}-byte image")]
    OutOfImage {
        offset: usize,
        len: usize,
        size: usize,
    },
    #[error("Game Genie code {code:?} {reason}")]
    Genie { code: String, reason: &'static str },
    #[error("patch pack {path}: {reason}")]
    Pack { path: PathBuf, reason: String },
    #[error("patch pack {name:?} is for the ROM with SHA-1 {expected}, not this one ({found})")]
    WrongRom {
        name: String,
        expected: Sha1Digest,
        found: Sha1Digest,
    },
    #[error("patch pack {name:?} should give the image with SHA-1 {expected}, not {found}")]
    WrongResult {
        name: String,
        expected: Sha1Digest,
        found: Sha1Digest,
    },
    #[error("patch pack {name:?}, patch {number}: {source}")]
    Entry {
        name: String,
        number: usize,
        source: Box<PatchError>,
    },
    #[error("the patches clash: {what}")]
    Clash { what: String },
    #[error("the patched image does not load: {0}")]
    Rom(#[from] RomError),
}

/// A patch file in one of the formats with an applier, told apart by its magic, never by
/// its file name.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PatchFile {
    Ips(Vec<u8>),
    Ups(Vec<u8>),
    Bps(Vec<u8>),
}

impl PatchFile {
    /// The patch in `bytes`, or `None` when they start with none of the formats' magics.
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        if bytes.starts_with(ips::MAGIC) {
            Some(Self::Ips(bytes))
        } else if bytes.starts_with(ups::MAGIC) {
            Some(Self::Ups(bytes))
        } else if bytes.starts_with(bps::MAGIC) {
            Some(Self::Bps(bytes))
        } else {
            None
        }
    }

    /// The patch file at `path`.
    ///
    /// # Errors
    /// [`PatchError::Io`] when it cannot be read, [`PatchError::UnknownFormat`] when it is in
    /// none of the formats.
    pub fn load(path: &Path) -> Result<Self, PatchError> {
        Self::from_bytes(read(path)?).ok_or_else(|| PatchError::UnknownFormat {
            path: path.to_owned(),
        })
    }

    /// The patch's format.
    pub fn format(&self) -> Format {
        match self {
            Self::Ips(_) => Format::Ips,
            Self::Ups(_) => Format::Ups,
            Self::Bps(_) => Format::Bps,
        }
    }

    /// `image` with the patch applied.
    ///
    /// # Errors
    /// A malformed or truncated patch, a patch for another image (UPS and BPS check their
    /// CRC-32s; IPS has none), or a result over the 4 MiB limit.
    pub fn apply(&self, image: &[u8]) -> Result<Vec<u8>, PatchError> {
        match self {
            Self::Ips(patch) => ips::apply(image, patch),
            Self::Ups(patch) => ups::apply(image, patch),
            Self::Bps(patch) => bps::apply(image, patch),
        }
    }
}

/// One patch the user picked: a pack, or a lone patch file.
#[derive(Debug, Clone)]
pub enum Patch {
    Pack(Pack),
    File { name: String, file: PatchFile },
}

impl Patch {
    /// The patch at `path`: a pack when it ends in `.toml`, else a patch file.
    ///
    /// # Errors
    /// The errors of [`Pack::load`] or [`PatchFile::load`].
    pub fn load(path: &Path) -> Result<Self, PatchError> {
        let is_pack = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
        if is_pack {
            return Ok(Self::Pack(Pack::load(path)?));
        }
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self::File {
            name,
            file: PatchFile::load(path)?,
        })
    }

    /// The name to show for the patch: the pack's name, or the patch file's stem.
    pub fn name(&self) -> &str {
        match self {
            Self::Pack(pack) => &pack.name,
            Self::File { name, .. } => name,
        }
    }
}

/// `rom` with `patches` applied in order, every pack checked against `rom`'s SHA-1. When
/// `rom`'s header checksum was right, the patched image gets its checksum rewritten, so a
/// game that checks itself at boot still starts.
///
/// # Errors
/// The first patch that fails, a lone pack whose `result` the image does not match, or a
/// patched image that does not load.
pub fn apply(rom: &Rom, patches: &[Patch]) -> Result<Rom, PatchError> {
    let mut image = rom.bytes().to_vec();
    for patch in patches {
        image = match patch {
            Patch::Pack(pack) => pack.apply(rom.sha1(), &image)?,
            Patch::File { file, .. } => file.apply(&image)?,
        };
    }
    let mut patched = Rom::from_bytes(image)?;
    if rom.has_valid_checksum() {
        patched = patched.with_fixed_checksum();
    }
    // A pack's result is what it makes of its ROM on its own, so it is checked only then.
    if let [Patch::Pack(pack)] = patches
        && let Some(expected) = pack.result
        && expected != patched.sha1()
    {
        return Err(PatchError::WrongResult {
            name: pack.name.clone(),
            expected,
            found: patched.sha1(),
        });
    }
    Ok(patched)
}

fn read(path: &Path) -> Result<Vec<u8>, PatchError> {
    fs::read(path).map_err(|source| PatchError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Fails unless an image of `size` bytes fits the 4 MiB the 68000 maps, so a patch can
/// never make the appliers allocate more.
fn check_size(size: u64) -> Result<usize, PatchError> {
    usize::try_from(size)
        .ok()
        .filter(|&size| size <= MAX_ROM_SIZE)
        .ok_or(PatchError::TooLarge { size })
}

/// Writes `bytes` over `image` at `offset`, which must lie wholly inside it.
fn overwrite(image: &mut [u8], offset: usize, bytes: &[u8]) -> Result<(), PatchError> {
    let size = image.len();
    let target = offset
        .checked_add(bytes.len())
        .and_then(|end| image.get_mut(offset..end))
        .ok_or(PatchError::OutOfImage {
            offset,
            len: bytes.len(),
            size,
        })?;
    target.copy_from_slice(bytes);
    Ok(())
}
