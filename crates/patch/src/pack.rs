//! Patch packs: a TOML file naming the ROM it is for by SHA-1 and an ordered list of
//! patches, each a patch file next to the pack, an inline byte run, or Game Genie codes.
//!
//! ```toml
//! name = "Example fixes"
//! rom = "<SHA-1 of the unpatched ROM>"
//! result = "<SHA-1 of the image this pack alone builds>"   # optional
//!
//! [[patch]]
//! file = "fix.bps"
//!
//! [[patch]]
//! offset = 0x01A2B4
//! bytes = "4E71 4E71"
//!
//! [[patch]]
//! genie = ["ABCD-EFGH"]
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
};

use anygenesis_rom::Sha1Digest;
use serde::Deserialize;

use crate::{GenieCode, PatchError, PatchFile, overwrite};

/// A patch pack as written in its file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackText {
    name: String,
    rom: String,
    result: Option<String>,
    #[serde(default)]
    patch: Vec<EntryText>,
}

/// One `[[patch]]` table: exactly one of `file`, `offset` with `bytes`, or `genie`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryText {
    file: Option<PathBuf>,
    offset: Option<u32>,
    bytes: Option<String>,
    genie: Option<Vec<String>>,
}

/// One patch in a pack.
#[derive(Debug, Clone)]
enum Entry {
    File(PatchFile),
    Bytes { offset: u32, bytes: Vec<u8> },
    Genie(Vec<GenieCode>),
}

/// A patch pack for one ROM.
#[derive(Debug, Clone)]
pub struct Pack {
    pub name: String,
    /// The SHA-1 of the unpatched ROM the pack is for.
    pub rom: Sha1Digest,
    /// The SHA-1 of the image the pack builds from its ROM when it is the only patch,
    /// header checksum fix-up included.
    pub result: Option<Sha1Digest>,
    entries: Vec<Entry>,
}

impl Pack {
    /// The pack in the file at `path`, its patch files read from the pack's folder.
    ///
    /// # Errors
    /// Reading the pack or one of its patch files, or [`PatchError::Pack`] for a pack that
    /// does not parse.
    pub fn load(path: &Path) -> Result<Self, PatchError> {
        let text = fs::read_to_string(path).map_err(|source| PatchError::Io {
            path: path.to_owned(),
            source,
        })?;
        let folder = path.parent().unwrap_or(Path::new(""));
        Self::parse(&text, folder).map_err(|error| match error {
            PackFault::Reason(reason) => PatchError::Pack {
                path: path.to_owned(),
                reason,
            },
            PackFault::Patch(error) => error,
        })
    }

    fn parse(text: &str, folder: &Path) -> Result<Self, PackFault> {
        let pack: PackText = toml::from_str(text).map_err(|error| error.to_string())?;
        let digest = |text: &str| {
            text.parse::<Sha1Digest>()
                .map_err(|error| error.to_string())
        };
        let entries = pack
            .patch
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                Entry::parse(entry, folder).map_err(|fault| match fault {
                    PackFault::Reason(reason) => {
                        PackFault::Reason(format!("patch {}: {reason}", index + 1))
                    }
                    PackFault::Patch(error) => PackFault::Patch(error),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            name: pack.name,
            rom: digest(&pack.rom)?,
            result: pack.result.as_deref().map(digest).transpose()?,
            entries,
        })
    }

    /// `image` with the pack's patches applied in order, after checking that `rom`, the
    /// SHA-1 of the unpatched ROM, is the pack's.
    ///
    /// # Errors
    /// [`PatchError::WrongRom`], or the first patch that fails, wrapped in
    /// [`PatchError::Entry`].
    pub fn apply(&self, rom: Sha1Digest, image: &[u8]) -> Result<Vec<u8>, PatchError> {
        if rom != self.rom {
            return Err(PatchError::WrongRom {
                name: self.name.clone(),
                expected: self.rom,
                found: rom,
            });
        }
        let mut image = image.to_vec();
        for (index, entry) in self.entries.iter().enumerate() {
            image = entry.apply(image).map_err(|source| PatchError::Entry {
                name: self.name.clone(),
                number: index + 1,
                source: Box::new(source),
            })?;
        }
        Ok(image)
    }
}

impl Entry {
    fn parse(entry: EntryText, folder: &Path) -> Result<Self, PackFault> {
        match entry {
            EntryText {
                file: Some(file),
                offset: None,
                bytes: None,
                genie: None,
            } => Ok(Self::File(PatchFile::load(&folder.join(file))?)),
            EntryText {
                file: None,
                offset: Some(offset),
                bytes: Some(bytes),
                genie: None,
            } => Ok(Self::Bytes {
                offset,
                bytes: parse_hex(&bytes)?,
            }),
            EntryText {
                file: None,
                offset: None,
                bytes: None,
                genie: Some(codes),
            } => {
                let codes = codes
                    .iter()
                    .map(|code| code.parse::<GenieCode>())
                    .collect::<Result<_, _>>()?;
                Ok(Self::Genie(codes))
            }
            _ => Err(PackFault::Reason(
                "give exactly one of `file`, `offset` with `bytes`, or `genie`".to_owned(),
            )),
        }
    }

    fn apply(&self, mut image: Vec<u8>) -> Result<Vec<u8>, PatchError> {
        match self {
            Self::File(file) => return file.apply(&image),
            Self::Bytes { offset, bytes } => overwrite(&mut image, *offset as usize, bytes)?,
            Self::Genie(codes) => {
                for code in codes {
                    code.apply(&mut image)?;
                }
            }
        }
        Ok(image)
    }
}

/// Why a pack did not parse: a fault in its text, or a patch in it that failed to load.
#[derive(Debug)]
enum PackFault {
    Reason(String),
    Patch(PatchError),
}

impl From<String> for PackFault {
    fn from(reason: String) -> Self {
        Self::Reason(reason)
    }
}

impl From<PatchError> for PackFault {
    fn from(error: PatchError) -> Self {
        Self::Patch(error)
    }
}

/// Bytes written as pairs of hex digits; whitespace is ignored.
fn parse_hex(text: &str) -> Result<Vec<u8>, PackFault> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let fault = || PackFault::Reason(format!("`bytes = {text:?}` is not whole hex byte pairs"));
    if digits.is_empty()
        || !digits.len().is_multiple_of(2)
        || !digits.iter().all(u8::is_ascii_hexdigit)
    {
        return Err(fault());
    }
    let value = |digit: u8| (digit as char).to_digit(16).unwrap_or_default() as u8;
    Ok(digits
        .chunks(2)
        .map(|pair| value(pair[0]) << 4 | value(pair[1]))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    const ROM: &[u8] = b"an image that stands in for a ROM";

    fn sha1(bytes: &[u8]) -> String {
        Sha1Digest::of(bytes).to_string()
    }

    fn parse(text: &str) -> Result<Pack, PackFault> {
        Pack::parse(text, Path::new(""))
    }

    fn reason(text: &str) -> String {
        match parse(text) {
            Err(PackFault::Reason(reason)) => reason,
            other => panic!("expected a pack fault, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_hex_whitespace_between_pairs_accepted() {
        let bytes = parse_hex(" 4E71\n4e 71 ").expect("hex");
        assert_eq!(bytes, [0x4E, 0x71, 0x4E, 0x71]);
        for bad in ["", " ", "4E7", "4G", "+1"] {
            assert!(parse_hex(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn test_pack_inline_bytes_applied_in_order() {
        let text = format!(
            "name = \"t\"\nrom = \"{}\"\n\n[[patch]]\noffset = 3\nbytes = \"41 42\"\n\n\
             [[patch]]\noffset = 4\nbytes = \"43\"\n",
            sha1(ROM)
        );
        let pack = parse(&text).expect("the pack parses");
        let image = pack.apply(Sha1Digest::of(ROM), ROM).expect("applies");
        assert_eq!(&image[..8], b"an ACage");
        assert_eq!(image.len(), ROM.len());
    }

    #[test]
    fn test_pack_other_rom_refused() {
        let text = format!("name = \"t\"\nrom = \"{}\"\n", sha1(b"another ROM"));
        let pack = parse(&text).expect("the pack parses");
        let error = pack.apply(Sha1Digest::of(ROM), ROM).expect_err("wrong ROM");
        assert!(matches!(error, PatchError::WrongRom { .. }), "{error}");
    }

    #[test]
    fn test_pack_write_past_the_end_refused() {
        let text = format!(
            "name = \"t\"\nrom = \"{}\"\n[[patch]]\noffset = {}\nbytes = \"0102\"\n",
            sha1(ROM),
            ROM.len() - 1
        );
        let pack = parse(&text).expect("the pack parses");
        let error = pack
            .apply(Sha1Digest::of(ROM), ROM)
            .expect_err("past the end");
        let PatchError::Entry { number, source, .. } = error else {
            panic!("expected an entry error, got {error}");
        };
        assert_eq!(number, 1);
        assert!(matches!(*source, PatchError::OutOfImage { .. }), "{source}");
    }

    #[test]
    fn test_pack_mixed_or_unknown_entry_fields_refused() {
        let head = format!("name = \"t\"\nrom = \"{}\"\n", sha1(ROM));
        let mixed = format!("{head}[[patch]]\noffset = 1\nbytes = \"00\"\ngenie = []\n");
        assert!(reason(&mixed).starts_with("patch 1: give exactly one"));
        let lone_offset = format!("{head}[[patch]]\noffset = 1\n");
        assert!(reason(&lone_offset).starts_with("patch 1: give exactly one"));
        assert!(reason(&format!("{head}[[patch]]\nadress = 1\n")).contains("adress"));
        assert!(reason(&format!("{head}colour = 1\n")).contains("colour"));
        assert!(reason("name = \"t\"\nrom = \"abc\"\n").contains("not a SHA-1"));
    }

    #[test]
    fn test_pack_load_reads_patch_files_beside_it() {
        let folder = env::temp_dir().join(format!("anygenesis-pack-test-{}", std::process::id()));
        fs::create_dir_all(&folder).expect("the test folder is created");
        // An IPS patch writing "XY" at offset 1.
        let ips = b"PATCH\x00\x00\x01\x00\x02XYEOF";
        fs::write(folder.join("fix.ips"), ips).expect("written");
        let pack_path = folder.join("pack.toml");
        let text = format!(
            "name = \"t\"\nrom = \"{}\"\n[[patch]]\nfile = \"fix.ips\"\n",
            sha1(ROM)
        );
        fs::write(&pack_path, text).expect("written");
        let pack = Pack::load(&pack_path).expect("the pack loads");
        let image = pack.apply(Sha1Digest::of(ROM), ROM).expect("applies");
        assert_eq!(&image[..4], b"aXYi");

        fs::write(folder.join("fix.ips"), b"not a patch").expect("written");
        let error = Pack::load(&pack_path).expect_err("unknown format");
        assert!(matches!(error, PatchError::UnknownFormat { .. }), "{error}");
        fs::remove_dir_all(&folder).expect("the test folder is removed");
    }
}
