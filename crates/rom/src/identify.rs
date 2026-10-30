use std::{fmt, str::FromStr};

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

/// Reads the 40 hex digits `Display` writes, in either case.
impl FromStr for Sha1Digest {
    type Err = ParseSha1Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = || ParseSha1Error {
            text: text.to_owned(),
        };
        if text.len() != 40 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(error());
        }
        let value = |digit: u8| (digit as char).to_digit(16).unwrap_or_default() as u8;
        let mut bytes = [0; 20];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
            *byte = value(pair[0]) << 4 | value(pair[1]);
        }
        Ok(Self(bytes))
    }
}

/// Text that is not a SHA-1 digest in hex.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a SHA-1 digest (40 hex digits)")]
pub struct ParseSha1Error {
    text: String,
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

    #[test]
    fn test_sha1_digest_from_str_hex_round_trips() {
        let digest = Sha1Digest::of(b"abc");
        assert_eq!(digest.to_string().parse::<Sha1Digest>(), Ok(digest));
        let upper = digest.to_string().to_uppercase();
        assert_eq!(upper.parse::<Sha1Digest>(), Ok(digest));
    }

    #[test]
    fn test_sha1_digest_from_str_bad_text_refused() {
        let digest = Sha1Digest::of(b"abc").to_string();
        let signed = format!("+{}", &digest[1..]);
        for text in [
            &digest[1..],
            &format!("{digest}0"),
            &digest.replace('a', "g"),
            &signed,
            "",
        ] {
            assert!(text.parse::<Sha1Digest>().is_err(), "{text:?} parsed");
        }
        assert!("é".repeat(20).parse::<Sha1Digest>().is_err());
    }
}
