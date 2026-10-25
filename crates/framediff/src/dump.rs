//! Machine state at one stop, and the files both runs keep it in.
//!
//! A run's folder holds one set of files per stop, numbered from 1: `<nnnnn>-ram.bin` (work
//! RAM), `-vram.bin`, `-cram.bin`, `-vsram.bin` and `-reg.bin` (the 24 VDP registers), plus
//! `<nnnnn>-note.txt` from our runs. The layout is what the reference emulator's debugger
//! `save` command writes: bytes as they are for RAM, VRAM and the registers, and CRAM and
//! VSRAM as little-endian 16-bit words. In memory every region is in 68000 byte order.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use crate::Error;

pub const RAM_SIZE: usize = 0x1_0000;
pub const VRAM_SIZE: usize = 0x1_0000;
/// 64 colours of two bytes.
pub const CRAM_SIZE: usize = 128;
/// 40 scroll values of two bytes.
pub const VSRAM_SIZE: usize = 80;
pub const REGISTERS: usize = 24;
/// Where work RAM starts in the 68000's address space.
pub const RAM_START: u32 = 0xFF_0000;

/// A part of the machine state, compared on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Ram,
    Vram,
    Cram,
    Vsram,
    Registers,
}

impl Region {
    pub const ALL: [Self; 5] = [
        Self::Ram,
        Self::Vram,
        Self::Cram,
        Self::Vsram,
        Self::Registers,
    ];

    /// Bytes the region holds.
    pub fn size(self) -> usize {
        match self {
            Self::Ram => RAM_SIZE,
            Self::Vram => VRAM_SIZE,
            Self::Cram => CRAM_SIZE,
            Self::Vsram => VSRAM_SIZE,
            Self::Registers => REGISTERS,
        }
    }

    /// The address of the region's first byte: work RAM by its 68000 address, everything
    /// else from 0 as the VDP addresses it.
    pub fn base(self) -> u32 {
        match self {
            Self::Ram => RAM_START,
            _ => 0,
        }
    }

    /// What the region's file name ends in.
    fn suffix(self) -> &'static str {
        match self {
            Self::Ram => "ram",
            Self::Vram => "vram",
            Self::Cram => "cram",
            Self::Vsram => "vsram",
            Self::Registers => "reg",
        }
    }

    /// Whether the file holds little-endian words.
    fn swapped(self) -> bool {
        matches!(self, Self::Cram | Self::Vsram)
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ram => "work RAM",
            Self::Vram => "VRAM",
            Self::Cram => "CRAM",
            Self::Vsram => "VSRAM",
            Self::Registers => "VDP registers",
        })
    }
}

/// The machine state at one stop, every region in 68000 byte order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dump {
    regions: [Vec<u8>; 5],
}

impl Default for Dump {
    fn default() -> Self {
        Self {
            regions: Region::ALL.map(|region| vec![0; region.size()]),
        }
    }
}

impl Dump {
    pub fn region(&self, region: Region) -> &[u8] {
        &self.regions[region as usize]
    }

    pub fn region_mut(&mut self, region: Region) -> &mut [u8] {
        &mut self.regions[region as usize]
    }

    /// Reads stop `index` from `folder`; `None` when the run has no such stop.
    ///
    /// # Errors
    /// A region file that cannot be read or has the wrong size.
    pub fn read(folder: &Path, index: u64) -> Result<Option<Self>, Error> {
        if !path(folder, index, Region::Ram).exists() {
            return Ok(None);
        }
        let mut dump = Self::default();
        for region in Region::ALL {
            let file = path(folder, index, region);
            let mut bytes = fs::read(&file).map_err(|source| Error::Io {
                path: file.clone(),
                source,
            })?;
            if bytes.len() != region.size() {
                return Err(Error::Size {
                    path: file,
                    size: bytes.len(),
                    expected: region.size(),
                });
            }
            if region.swapped() {
                swap_words(&mut bytes);
            }
            dump.regions[region as usize] = bytes;
        }
        Ok(Some(dump))
    }

    /// Writes the dump as stop `index` into `folder`, which must exist.
    ///
    /// # Errors
    /// A file that cannot be written.
    pub fn write(&self, folder: &Path, index: u64) -> Result<(), Error> {
        for region in Region::ALL {
            let mut bytes = self.region(region).to_vec();
            if region.swapped() {
                swap_words(&mut bytes);
            }
            let file = path(folder, index, region);
            fs::write(&file, bytes).map_err(|source| Error::Io { path: file, source })?;
        }
        Ok(())
    }
}

/// Writes the note for stop `index`: what our run knows about it that the dump does not hold.
///
/// # Errors
/// A file that cannot be written.
pub fn write_note(folder: &Path, index: u64, note: &str) -> Result<(), Error> {
    let file = note_path(folder, index);
    fs::write(&file, note).map_err(|source| Error::Io { path: file, source })
}

/// The note for stop `index`, if the run left one.
///
/// # Errors
/// A note that exists but cannot be read.
pub fn read_note(folder: &Path, index: u64) -> Result<Option<String>, Error> {
    let file = note_path(folder, index);
    match fs::read_to_string(&file) {
        Ok(note) => Ok(Some(note)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { path: file, source }),
    }
}

/// The name of a region's file for stop `index`, as the reference's script saves it.
pub fn file_name(index: u64, region: Region) -> String {
    format!("{index:05}-{}.bin", region.suffix())
}

fn path(folder: &Path, index: u64, region: Region) -> PathBuf {
    folder.join(file_name(index, region))
}

fn note_path(folder: &Path, index: u64) -> PathBuf {
    folder.join(format!("{index:05}-note.txt"))
}

fn swap_words(bytes: &mut [u8]) {
    for word in bytes.as_chunks_mut::<2>().0 {
        word.swap(0, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Scratch;

    #[test]
    fn test_dump_write_then_read_returns_the_same_state() {
        let scratch = Scratch::new("dump-round-trip");
        let mut dump = Dump::default();
        dump.region_mut(Region::Ram)[0x10] = 0xAB;
        dump.region_mut(Region::Cram)[..2].copy_from_slice(&[0x0E, 0x02]);
        dump.region_mut(Region::Registers)[23] = 0x7F;
        dump.write(scratch.path(), 7).expect("written");
        assert_eq!(Dump::read(scratch.path(), 7).expect("read"), Some(dump));
        assert_eq!(Dump::read(scratch.path(), 8).expect("read"), None);
    }

    #[test]
    fn test_dump_cram_file_holds_little_endian_words() {
        let scratch = Scratch::new("dump-cram-order");
        let mut dump = Dump::default();
        dump.region_mut(Region::Cram)[..2].copy_from_slice(&[0x0E, 0x02]);
        dump.region_mut(Region::Vram)[..2].copy_from_slice(&[0x12, 0x34]);
        dump.write(scratch.path(), 1).expect("written");
        let cram = fs::read(scratch.path().join("00001-cram.bin")).expect("read");
        let vram = fs::read(scratch.path().join("00001-vram.bin")).expect("read");
        assert_eq!(
            (&cram[..2], &vram[..2]),
            (&[0x02, 0x0E][..], &[0x12, 0x34][..])
        );
    }

    #[test]
    fn test_dump_read_wrong_size_is_an_error() {
        let scratch = Scratch::new("dump-size");
        Dump::default().write(scratch.path(), 1).expect("written");
        fs::write(scratch.path().join("00001-vsram.bin"), [0; 128]).expect("written");
        let error = Dump::read(scratch.path(), 1).expect_err("refused");
        assert!(
            matches!(
                error,
                Error::Size {
                    size: 128,
                    expected: 80,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn test_note_missing_reads_as_none() {
        let scratch = Scratch::new("dump-note");
        assert_eq!(read_note(scratch.path(), 1).expect("read"), None);
        write_note(scratch.path(), 1, "pc $000B10\n").expect("written");
        assert_eq!(
            read_note(scratch.path(), 1).expect("read").as_deref(),
            Some("pc $000B10\n")
        );
    }
}
