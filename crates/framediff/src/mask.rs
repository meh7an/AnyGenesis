//! Masks: address ranges the diff leaves out, each signed off with a reason.
//!
//! A masks file is TOML with one `[[mask]]` table per range:
//!
//! ```toml
//! [[mask]]
//! region = "ram"          # ram, vram, cram, vsram or registers
//! start = 0xFFFDE8        # work RAM by 68000 address, the rest from 0
//! end = 0xFFFDF0          # exclusive
//! reason = "why the difference is expected"
//! ```

use std::{fs, ops::Range, path::Path};

use serde::Deserialize;

use crate::{Error, dump::Region};

/// One masked range.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub region: Region,
    pub start: u32,
    pub end: u32,
    pub reason: String,
}

impl Mask {
    /// The masked byte offsets within the region.
    fn offsets(&self) -> Range<usize> {
        let base = self.region.base();
        (self.start - base) as usize..(self.end - base) as usize
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    mask: Vec<Mask>,
}

/// The masks in force for a diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Masks {
    masks: Vec<Mask>,
}

impl Masks {
    /// Parses a masks file.
    ///
    /// # Errors
    /// TOML that does not fit the format, or a range outside its region or empty.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let file: File = toml::from_str(text).map_err(|error| Error::Masks(error.to_string()))?;
        for mask in &file.mask {
            let base = mask.region.base();
            let limit = u64::from(base) + mask.region.size() as u64;
            let inside = mask.start >= base && u64::from(mask.end) <= limit;
            if !inside || mask.start >= mask.end {
                return Err(Error::Masks(format!(
                    "the {} mask ${:06X}-${:06X} is empty or outside ${base:06X}-${limit:06X}",
                    mask.region, mask.start, mask.end
                )));
            }
            if mask.reason.trim().is_empty() {
                return Err(Error::Masks(format!(
                    "the {} mask ${:06X}-${:06X} has no reason",
                    mask.region, mask.start, mask.end
                )));
            }
        }
        Ok(Self { masks: file.mask })
    }

    /// Reads and parses a masks file.
    ///
    /// # Errors
    /// A file that cannot be read, or what [`Masks::parse`] refuses.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text)
    }

    pub fn masks(&self) -> &[Mask] {
        &self.masks
    }

    /// Whether the byte at `offset` in `region` is masked.
    pub fn covers(&self, region: Region, offset: usize) -> bool {
        self.masks
            .iter()
            .any(|mask| mask.region == region && mask.offsets().contains(&offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
[[mask]]
region = "ram"
start = 0xFFFDE8
end = 0xFFFDF0
reason = "stale interrupt frames"

[[mask]]
region = "cram"
start = 0
end = 2
reason = "backdrop"
"#;

    #[test]
    fn test_masks_parse_covers_ranges_by_region() {
        let masks = Masks::parse(FILE).expect("ok");
        assert_eq!(masks.masks().len(), 2);
        assert!(masks.covers(Region::Ram, 0xFDE8));
        assert!(masks.covers(Region::Ram, 0xFDEF));
        assert!(!masks.covers(Region::Ram, 0xFDF0));
        assert!(masks.covers(Region::Cram, 1));
        assert!(!masks.covers(Region::Vram, 1));
    }

    #[test]
    fn test_masks_parse_empty_file_masks_nothing() {
        assert!(Masks::parse("").expect("ok").masks().is_empty());
    }

    #[test]
    fn test_masks_parse_bad_ranges_refused() {
        let ranges = [
            ("ram", 0xFF_0000, 0xFF_0000),
            ("ram", 0x1000, 0x1002),
            ("vsram", 0, 82),
            ("registers", 20, 25),
        ];
        for (region, start, end) in ranges {
            let text = format!(
                "[[mask]]\nregion = \"{region}\"\nstart = {start}\nend = {end}\nreason = \"x\"\n"
            );
            assert!(Masks::parse(&text).is_err(), "{text}");
        }
    }

    #[test]
    fn test_masks_parse_without_reason_refused() {
        let text = "[[mask]]\nregion = \"ram\"\nstart = 0xFF0000\nend = 0xFF0002\n";
        assert!(matches!(Masks::parse(text), Err(Error::Masks(_))));
    }
}
