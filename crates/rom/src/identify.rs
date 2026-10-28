use std::fmt;

/// A SHA-1 digest of a whole ROM image, the key for anything tied to one image (tests,
/// patch packs).
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
}
