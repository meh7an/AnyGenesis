use std::fmt;

/// A SHA-1 digest of a whole ROM image, the key every game config uses.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha1Digest([u8; 20]);

impl Sha1Digest {
    /// Hashes a ROM image.
    pub fn of(bytes: &[u8]) -> Self {
        Self(sha1_smol::Sha1::from(bytes).digest().bytes())
    }

    /// Returns the raw digest.
    pub fn bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Display for Sha1Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// Hex, like `Display`, so digests in test failures can be compared with published hashes.
impl fmt::Debug for Sha1Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha1Digest({self})")
    }
}

/// A ROM image AnyGenesis supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownRom {
    /// Game and revision.
    pub title: &'static str,
    /// Lowercase hex SHA-1 of the whole `.bin` image.
    pub sha1: &'static str,
}

/// Every supported ROM.
pub const KNOWN_ROMS: [KnownRom; 1] = [KnownRom {
    title: "Sonic the Hedgehog (REV00)",
    // Sega Retro, "Sonic the Hedgehog (Mega Drive)/Technical information", Revision 00.
    sha1: "6ddb7de1e17e7f6cdb88927bd906352030daa194",
}];

/// Returns the supported ROM with this digest.
pub(crate) fn find_known(digest: Sha1Digest) -> Option<KnownRom> {
    let hex = digest.to_string();
    KNOWN_ROMS.into_iter().find(|known| known.sha1 == hex)
}

/// Lists the supported ROMs for error messages.
pub(crate) fn supported_list() -> String {
    KNOWN_ROMS
        .iter()
        .map(|known| format!("{} (SHA-1 {})", known.title, known.sha1))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha1_digest_fips_vector_matches() {
        let expected = "a9993e364706816aba3e25717850c26c9cd0d89d";
        assert_eq!(Sha1Digest::of(b"abc").to_string(), expected);
    }

    #[test]
    fn test_sha1_digest_debug_shows_hex() {
        let expected = "Sha1Digest(a9993e364706816aba3e25717850c26c9cd0d89d)";
        assert_eq!(format!("{:?}", Sha1Digest::of(b"abc")), expected);
    }

    #[test]
    fn test_known_roms_sha1_is_lowercase_hex() {
        for known in KNOWN_ROMS {
            assert_eq!(known.sha1.len(), 40);
            let is_lower_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
            assert!(known.sha1.bytes().all(is_lower_hex));
        }
    }

    #[test]
    fn test_find_known_unknown_digest_none() {
        assert_eq!(find_known(Sha1Digest::of(b"not a ROM")), None);
    }
}
