use std::fmt;

/// A position in a config file, 1-based. Line 0 marks an entry built in code, not parsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    pub line: u32,
    pub column: u32,
}

impl Location {
    /// Returns the line and column of byte `offset` in `text`, counting columns in characters.
    pub(crate) fn of_offset(text: &str, offset: usize) -> Self {
        let before = text.get(..offset).unwrap_or(text);
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        Self {
            line: count_u32(before.matches('\n').count()) + 1,
            column: count_u32(before[line_start..].chars().count()) + 1,
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

fn count_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_location_of_offset_first_byte_is_line_one_column_one() {
        assert_eq!(
            Location::of_offset("abc", 0),
            Location { line: 1, column: 1 }
        );
    }

    #[test]
    fn test_location_of_offset_after_newlines_counts_lines_and_columns() {
        let text = "a = 1\nbb = 2\nccc = 3";
        assert_eq!(
            Location::of_offset(text, 13),
            Location { line: 3, column: 1 }
        );
        assert_eq!(
            Location::of_offset(text, 9),
            Location { line: 2, column: 4 }
        );
    }

    #[test]
    fn test_location_of_offset_past_end_clamps_to_end() {
        assert_eq!(
            Location::of_offset("ab\n", 50),
            Location { line: 2, column: 1 }
        );
    }
}
