//! Genesis ROM image loading and identification.
//!
//! Only raw `.bin` images load. Interleaved `.smd` dumps are detected and refused, and any
//! well-formed image parses; an image is identified by its SHA-1, never checked against a
//! list.

mod header;
mod identify;
mod smd;
mod vectors;

use std::{fmt, fs, io, path::Path};

use anygenesis_shared::{BoundsError, read_u16_be};

pub use header::{Header, Regions};
pub use identify::{ParseSha1Error, Sha1Digest};
pub use vectors::{VectorTable, vector_name};

use crate::header::{CHECKSUM, HEADER_END, HEADER_START};

/// The 68000 maps cartridge ROM at 0x000000-0x3FFFFF; anything larger needs a mapper.
pub const MAX_ROM_SIZE: usize = 0x40_0000;

/// A loaded ROM image with its parsed header and vector table.
#[derive(Debug, Clone)]
pub struct Rom {
    bytes: Vec<u8>,
    header: Header,
    vectors: VectorTable,
    computed_checksum: u16,
    sha1: Sha1Digest,
    warnings: Vec<RomWarning>,
}

impl Rom {
    /// Reads and parses a `.bin` image.
    ///
    /// # Errors
    /// Returns [`RomError::Io`] when the file cannot be read, and the errors of
    /// [`Rom::from_bytes`] otherwise.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RomError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| RomError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_bytes(bytes)
    }

    /// Parses a `.bin` image.
    ///
    /// Problems that do not stop the image from running, such as a wrong header checksum,
    /// are collected in [`Rom::warnings`] instead of failing.
    ///
    /// # Errors
    /// Returns [`RomError::InterleavedSmd`] for an `.smd` dump, and [`RomError::OddSize`],
    /// [`RomError::TooSmall`] or [`RomError::TooLarge`] when the size cannot be a cartridge.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, RomError> {
        let size = bytes.len();
        if smd::is_interleaved(&bytes) {
            return Err(RomError::InterleavedSmd { size });
        }
        if !size.is_multiple_of(2) {
            return Err(RomError::OddSize { size });
        }
        if size < HEADER_END {
            return Err(RomError::TooSmall { size });
        }
        if size > MAX_ROM_SIZE {
            return Err(RomError::TooLarge { size });
        }

        let header = Header::parse(&bytes)?;
        let vectors = VectorTable::parse(&bytes)?;
        let computed_checksum = checksum(&bytes)?;
        let warnings = collect_warnings(&bytes, &header, computed_checksum);
        Ok(Self {
            sha1: Sha1Digest::of(&bytes),
            bytes,
            header,
            vectors,
            computed_checksum,
            warnings,
        })
    }

    /// Returns the whole image.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the parsed cartridge header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Returns the 68000 exception vectors.
    pub fn vectors(&self) -> &VectorTable {
        &self.vectors
    }

    /// Returns the wrapping sum of every word from 0x200 to the end of the file.
    pub fn computed_checksum(&self) -> u16 {
        self.computed_checksum
    }

    /// Whether the header checksum agrees with the computed one.
    pub fn has_valid_checksum(&self) -> bool {
        self.header.checksum == self.computed_checksum
    }

    /// Returns the image with the computed checksum written into its header, so a game that
    /// checks itself at boot accepts a changed image.
    #[must_use]
    pub fn with_fixed_checksum(self) -> Self {
        let mut bytes = self.bytes;
        bytes[CHECKSUM..CHECKSUM + 2].copy_from_slice(&self.computed_checksum.to_be_bytes());
        // The image parsed before and the checksum field lies outside the words it sums.
        Self::from_bytes(bytes).expect("an image that parsed still parses")
    }

    /// Returns the SHA-1 of the whole image.
    pub fn sha1(&self) -> Sha1Digest {
        self.sha1
    }

    /// Returns the problems found while parsing, none of which stop the image from loading.
    pub fn warnings(&self) -> &[RomWarning] {
        &self.warnings
    }

    /// Returns the name to show for the game: the header's overseas title with runs of
    /// spaces collapsed, else its domestic title, else the start of the SHA-1.
    pub fn title(&self) -> String {
        [&self.header.overseas_title, &self.header.domestic_title]
            .into_iter()
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .find(|title| !title.is_empty())
            .unwrap_or_else(|| format!("SHA-1 {}", &self.sha1.to_string()[..8]))
    }
}

/// Why a ROM image was refused.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RomError {
    #[error("reading {path}: {source}")]
    Io { path: String, source: io::Error },
    #[error(
        "this {size}-byte file is an interleaved .smd dump; AnyGenesis loads raw .bin images \
         only, so convert it to .bin first"
    )]
    InterleavedSmd { size: usize },
    #[error("the image is {size} bytes, an odd size; a .bin image holds whole 16-bit words")]
    OddSize { size: usize },
    #[error(
        "the image is {size} bytes, too small to hold the vector table and header \
         ({:#x} bytes)",
        HEADER_END
    )]
    TooSmall { size: usize },
    #[error(
        "the image is {size} bytes, larger than the 4 MiB the 68000 maps without a mapper, \
         which AnyGenesis does not support"
    )]
    TooLarge { size: usize },
    #[error("the image ends inside its header: {0}")]
    Truncated(#[from] BoundsError),
}

/// A problem that does not stop a ROM image from loading.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RomWarning {
    /// The header does not start with `SEGA`, so a TMSS console would refuse to boot it.
    MissingSignature,
    /// The header checksum disagrees with the computed one.
    ChecksumMismatch { header: u16, computed: u16 },
    /// The header's ROM end is not the last byte of the file; the file size wins.
    RomEndMismatch { header_end: u32, file_end: u32 },
    /// The region code is in neither known style.
    UnknownRegion { code: String },
}

impl fmt::Display for RomWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSignature => write!(f, "the header does not start with SEGA"),
            Self::ChecksumMismatch { header, computed } => write!(
                f,
                "header checksum {header:#06x} differs from the computed {computed:#06x}"
            ),
            Self::RomEndMismatch {
                header_end,
                file_end,
            } => write!(
                f,
                "header ROM end {header_end:#x} differs from the file's last byte {file_end:#x}"
            ),
            Self::UnknownRegion { code } => write!(f, "unrecognized region code {code:?}"),
        }
    }
}

fn checksum(bytes: &[u8]) -> Result<u16, BoundsError> {
    (HEADER_END..bytes.len())
        .step_by(2)
        .try_fold(0u16, |sum, offset| {
            Ok(sum.wrapping_add(read_u16_be(bytes, offset)?))
        })
}

fn collect_warnings(bytes: &[u8], header: &Header, computed_checksum: u16) -> Vec<RomWarning> {
    let mut warnings = Vec::new();
    if !header::has_signature(&bytes[HEADER_START..]) {
        warnings.push(RomWarning::MissingSignature);
    }
    if header.checksum != computed_checksum {
        warnings.push(RomWarning::ChecksumMismatch {
            header: header.checksum,
            computed: computed_checksum,
        });
    }
    // The size checks in `from_bytes` keep the file under 4 MiB, so its end fits in u32.
    let file_end = (bytes.len() - 1) as u32;
    if *header.rom_range.end() != file_end {
        warnings.push(RomWarning::RomEndMismatch {
            header_end: *header.rom_range.end(),
            file_end,
        });
    }
    if header.regions.is_none() {
        warnings.push(RomWarning::UnknownRegion {
            code: header.region_code.clone(),
        });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: usize = 0x8000;

    /// Builds a well-formed synthetic image: made-up header text, every vector pointing at
    /// 0x200 except the SSP, and a correct checksum.
    fn synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0; SIZE];
        for index in 0..VectorTable::LEN {
            put_u32(&mut rom, index * 4, 0x0000_0200);
        }
        put_u32(&mut rom, 0, 0x00FF_FE00);
        put_u32(&mut rom, 30 * 4, 0x0000_0400);
        rom[0x100..0x200].fill(b' ');
        put_text(&mut rom, 0x100, "SEGA GENESIS");
        put_text(&mut rom, 0x110, "(C)TEST 2026.OCT");
        put_text(&mut rom, 0x120, "ANYGENESIS DOMESTIC");
        put_text(&mut rom, 0x150, "ANYGENESIS OVERSEAS");
        put_text(&mut rom, 0x180, "GM 12345678-00");
        put_u32(&mut rom, 0x1A0, 0);
        put_u32(&mut rom, 0x1A4, (SIZE - 1) as u32);
        put_u32(&mut rom, 0x1A8, 0x00FF_0000);
        put_u32(&mut rom, 0x1AC, 0x00FF_FFFF);
        put_text(&mut rom, 0x1F0, "JUE");
        for (offset, byte) in rom.iter_mut().enumerate().skip(0x200) {
            *byte = (offset % 251) as u8;
        }
        fix_checksum(&mut rom);
        rom
    }

    fn put_u32(rom: &mut [u8], offset: usize, value: u32) {
        rom[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_text(rom: &mut [u8], offset: usize, text: &str) {
        rom[offset..offset + text.len()].copy_from_slice(text.as_bytes());
    }

    fn fix_checksum(rom: &mut [u8]) {
        let sum = rom[0x200..].chunks(2).fold(0u16, |sum, word| {
            sum.wrapping_add(u16::from_be_bytes([word[0], word[1]]))
        });
        rom[0x18E..0x190].copy_from_slice(&sum.to_be_bytes());
    }

    fn load(bytes: Vec<u8>) -> Rom {
        Rom::from_bytes(bytes).expect("synthetic image is well-formed")
    }

    #[test]
    fn test_from_bytes_synthetic_rom_parses_header() {
        let rom = load(synthetic_rom());
        let header = rom.header();
        assert_eq!(header.system_type, "SEGA GENESIS");
        assert_eq!(header.copyright, "(C)TEST 2026.OCT");
        assert_eq!(header.domestic_title, "ANYGENESIS DOMESTIC");
        assert_eq!(header.overseas_title, "ANYGENESIS OVERSEAS");
        assert_eq!(header.serial, "GM 12345678-00");
        assert_eq!(header.rom_range, 0..=0x7FFF);
        assert_eq!(header.ram_range, 0x00FF_0000..=0x00FF_FFFF);
        assert_eq!(header.region_code, "JUE");
        let regions = header.regions.expect("JUE decodes");
        assert!(regions.japan && regions.americas && regions.europe && !regions.asia);
    }

    #[test]
    fn test_from_bytes_synthetic_rom_no_warnings() {
        let rom = load(synthetic_rom());
        assert_eq!(rom.warnings(), []);
        assert_eq!(rom.computed_checksum(), rom.header().checksum);
    }

    #[test]
    fn test_with_fixed_checksum_changed_image_checksum_rewritten() {
        let mut bytes = synthetic_rom();
        bytes[0x300] ^= 0xFF;
        let rom = load(bytes);
        assert!(!rom.has_valid_checksum());
        let fixed = rom.with_fixed_checksum();
        assert!(fixed.has_valid_checksum());
        assert_eq!(fixed.warnings(), []);
        assert_eq!(fixed.bytes()[0x300], (0x300 % 251) as u8 ^ 0xFF);
    }

    #[test]
    fn test_from_bytes_synthetic_rom_decodes_vectors() {
        let rom = load(synthetic_rom());
        assert_eq!(rom.vectors().initial_ssp(), 0x00FF_FE00);
        assert_eq!(rom.vectors().reset_pc(), 0x200);
        assert_eq!(rom.vectors().autovector(6), Some(0x400));
        assert_eq!(rom.vectors().autovector(4), Some(0x200));
    }

    #[test]
    fn test_checksum_known_words_wrapping_sum() {
        let mut bytes = vec![0; 0x206];
        bytes[0x200..].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x02, 0x12, 0x34]);
        assert_eq!(checksum(&bytes), Ok(0x1235));
    }

    #[test]
    fn test_from_bytes_bad_checksum_warns_and_loads() {
        let mut bytes = synthetic_rom();
        bytes[0x300] = bytes[0x300].wrapping_add(1);
        let rom = load(bytes);
        let header = rom.header().checksum;
        let expected = RomWarning::ChecksumMismatch {
            header,
            computed: header.wrapping_add(0x0100),
        };
        assert_eq!(rom.warnings(), [expected]);
    }

    #[test]
    fn test_from_bytes_rom_end_disagrees_warns_file_size_wins() {
        let mut bytes = synthetic_rom();
        put_u32(&mut bytes, 0x1A4, 0x0003_FFFF);
        let rom = load(bytes);
        let expected = RomWarning::RomEndMismatch {
            header_end: 0x0003_FFFF,
            file_end: 0x7FFF,
        };
        assert_eq!(rom.warnings(), [expected]);
        assert_eq!(rom.bytes().len(), SIZE);
    }

    #[test]
    fn test_from_bytes_missing_signature_and_region_warn() {
        let mut bytes = synthetic_rom();
        put_text(&mut bytes, 0x100, "NOT A CONSOLE");
        put_text(&mut bytes, 0x1F0, "XYZ");
        let rom = load(bytes);
        let expected = [
            RomWarning::MissingSignature,
            RomWarning::UnknownRegion {
                code: "XYZ".to_owned(),
            },
        ];
        assert_eq!(rom.warnings(), expected);
    }

    #[test]
    fn test_from_bytes_odd_size_refused() {
        let mut bytes = synthetic_rom();
        bytes.push(0);
        let result = Rom::from_bytes(bytes);
        assert!(matches!(result, Err(RomError::OddSize { size }) if size == SIZE + 1));
    }

    #[test]
    fn test_from_bytes_shorter_than_header_refused() {
        let result = Rom::from_bytes(vec![0; 0x1FE]);
        assert!(matches!(result, Err(RomError::TooSmall { size: 0x1FE })));
    }

    #[test]
    fn test_from_bytes_over_four_mib_refused() {
        let result = Rom::from_bytes(vec![0; MAX_ROM_SIZE + 2]);
        assert!(matches!(result, Err(RomError::TooLarge { .. })));
    }

    #[test]
    fn test_from_bytes_interleaved_smd_refused_with_message() {
        let smd = smd::tests::interleave(&synthetic_rom(), true);
        let error = Rom::from_bytes(smd).expect_err(".smd must be refused");
        assert!(matches!(error, RomError::InterleavedSmd { .. }));
        assert!(error.to_string().contains("convert it to .bin"));
    }

    #[test]
    fn test_from_bytes_interleaved_smd_without_copier_magic_refused() {
        let smd = smd::tests::interleave(&synthetic_rom(), false);
        let result = Rom::from_bytes(smd);
        assert!(matches!(result, Err(RomError::InterleavedSmd { .. })));
    }

    #[test]
    fn test_title_overseas_title_wins() {
        assert_eq!(load(synthetic_rom()).title(), "ANYGENESIS OVERSEAS");
    }

    #[test]
    fn test_title_runs_of_spaces_collapse() {
        let mut bytes = synthetic_rom();
        bytes[0x150..0x180].fill(b' ');
        put_text(&mut bytes, 0x150, "SONIC THE");
        put_text(&mut bytes, 0x168, "HEDGEHOG");
        assert_eq!(load(bytes).title(), "SONIC THE HEDGEHOG");
    }

    #[test]
    fn test_title_blank_overseas_falls_back_to_domestic() {
        let mut bytes = synthetic_rom();
        bytes[0x150..0x180].fill(b' ');
        assert_eq!(load(bytes).title(), "ANYGENESIS DOMESTIC");
    }

    #[test]
    fn test_title_blank_titles_fall_back_to_sha1() {
        let mut bytes = synthetic_rom();
        bytes[0x120..0x180].fill(b' ');
        let rom = load(bytes);
        let expected = format!("SHA-1 {}", &rom.sha1().to_string()[..8]);
        assert_eq!(rom.title(), expected);
    }

    #[test]
    fn test_load_missing_file_returns_io_error() {
        let result = Rom::load("this/path/does/not/exist.bin");
        assert!(matches!(result, Err(RomError::Io { .. })));
    }
}
