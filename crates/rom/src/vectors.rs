use anygenesis_shared::{BoundsError, read_u32_be};

/// The 68000 exception vector table at the start of the ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorTable {
    entries: [u32; Self::LEN],
}

impl VectorTable {
    /// Number of vectors the 68000 defines.
    pub const LEN: usize = 64;

    const INITIAL_SSP: usize = 0;
    const RESET_PC: usize = 1;
    /// The level n autovector sits n entries past this one.
    const SPURIOUS_INTERRUPT: usize = 24;
    const FIRST_TRAP: usize = 32;

    /// Decodes the table from the first 256 bytes of a ROM image.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, BoundsError> {
        let mut entries = [0; Self::LEN];
        for (index, entry) in entries.iter_mut().enumerate() {
            *entry = read_u32_be(bytes, index * 4)?;
        }
        Ok(Self { entries })
    }

    /// Returns all 64 raw entries, indexed by vector number.
    pub fn entries(&self) -> &[u32; Self::LEN] {
        &self.entries
    }

    /// Returns the supervisor stack pointer loaded at reset.
    pub fn initial_ssp(&self) -> u32 {
        self.entries[Self::INITIAL_SSP]
    }

    /// Returns the program counter loaded at reset.
    pub fn reset_pc(&self) -> u32 {
        self.entries[Self::RESET_PC]
    }

    /// Returns the handler for interrupt `level`, or `None` outside 1-7.
    ///
    /// On the Genesis, level 2 is the external interrupt, 4 HBlank and 6 VBlank.
    pub fn autovector(&self, level: u8) -> Option<u32> {
        (1..=7)
            .contains(&level)
            .then(|| self.entries[Self::SPURIOUS_INTERRUPT + usize::from(level)])
    }

    /// Returns the handler for `TRAP #number`, or `None` outside 0-15.
    pub fn trap(&self, number: u8) -> Option<u32> {
        (number < 16).then(|| self.entries[Self::FIRST_TRAP + usize::from(number)])
    }
}

/// Returns the 68000's name for vector `index`, or `None` past the table.
///
/// Entries the 68000 leaves unassigned, or that only later CPUs use, are `reserved`.
pub fn vector_name(index: usize) -> Option<&'static str> {
    VECTOR_NAMES.get(index).copied()
}

// Four per line, so each row is one aligned group of vectors.
#[rustfmt::skip]
const VECTOR_NAMES: [&str; VectorTable::LEN] = [
    "initial SSP", "reset PC", "bus error", "address error",
    "illegal instruction", "zero divide", "CHK", "TRAPV",
    "privilege violation", "trace", "line 1010", "line 1111",
    "reserved", "reserved", "reserved", "uninitialized interrupt",
    "reserved", "reserved", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved",
    "spurious interrupt", "level 1 autovector", "level 2 autovector", "level 3 autovector",
    "level 4 autovector", "level 5 autovector", "level 6 autovector", "level 7 autovector",
    "TRAP #0", "TRAP #1", "TRAP #2", "TRAP #3",
    "TRAP #4", "TRAP #5", "TRAP #6", "TRAP #7",
    "TRAP #8", "TRAP #9", "TRAP #10", "TRAP #11",
    "TRAP #12", "TRAP #13", "TRAP #14", "TRAP #15",
    "reserved", "reserved", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every vector holds `0x1000 + index`.
    fn indexed_table() -> VectorTable {
        let bytes: Vec<u8> = (0..VectorTable::LEN as u32)
            .flat_map(|index| (0x1000 + index).to_be_bytes())
            .collect();
        VectorTable::parse(&bytes).expect("256 bytes hold the whole table")
    }

    #[test]
    fn test_vector_table_parse_short_input_returns_bounds_error() {
        assert!(VectorTable::parse(&[0; 0xFC]).is_err());
    }

    #[test]
    fn test_vector_table_named_entries_read_their_index() {
        let table = indexed_table();
        assert_eq!(table.initial_ssp(), 0x1000);
        assert_eq!(table.reset_pc(), 0x1001);
        assert_eq!(table.entries()[63], 0x103F);
    }

    #[test]
    fn test_vector_table_autovector_levels_map_to_25_through_31() {
        let table = indexed_table();
        assert_eq!(table.autovector(1), Some(0x1019));
        assert_eq!(table.autovector(6), Some(0x101E));
        assert_eq!(table.autovector(7), Some(0x101F));
        assert_eq!(table.autovector(0), None);
        assert_eq!(table.autovector(8), None);
    }

    #[test]
    fn test_vector_table_trap_numbers_map_to_32_through_47() {
        let table = indexed_table();
        assert_eq!(table.trap(0), Some(0x1020));
        assert_eq!(table.trap(15), Some(0x102F));
        assert_eq!(table.trap(16), None);
    }

    #[test]
    fn test_vector_name_known_indexes_named() {
        assert_eq!(vector_name(1), Some("reset PC"));
        assert_eq!(vector_name(30), Some("level 6 autovector"));
        assert_eq!(vector_name(47), Some("TRAP #15"));
        assert_eq!(vector_name(64), None);
    }
}
