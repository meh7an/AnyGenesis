//! The input replay format both runs read.
//!
//! One change per line, `<stop> <buttons>`: from stop `<stop>` on (the n-th vertical
//! interrupt taken, counted from 1) pad 1 holds `<buttons>`, until the next line. Buttons are
//! letters from `UDLRABCS` (up, down, left, right, A, B, C, Start) or `-` for none. Stops
//! rise from line to line. `#` starts a comment; blank lines are ignored. Before the first
//! line nothing is held.

use std::{fmt, fs, path::Path};

use crate::Error;

/// Held buttons, one bit each in the runtime pad's order: up, down, left, right, A, B, C,
/// Start from bit 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons(pub u8);

impl Buttons {
    /// Letters in bit order.
    const LETTERS: [char; 8] = ['U', 'D', 'L', 'R', 'A', 'B', 'C', 'S'];
    /// The reference emulator's binding names, in bit order.
    const BINDINGS: [&str; 8] = ["up", "down", "left", "right", "a", "b", "c", "start"];

    fn parse(text: &str) -> Option<Self> {
        if text == "-" {
            return Some(Self(0));
        }
        text.chars().try_fold(Self(0), |held, letter| {
            let bit = Self::LETTERS.iter().position(|&known| known == letter)?;
            let mask = 1 << bit;
            (held.0 & mask == 0).then_some(Self(held.0 | mask))
        })
    }

    /// The binding names of the buttons held, in bit order.
    pub fn bindings(self) -> impl Iterator<Item = &'static str> {
        Self::BINDINGS
            .into_iter()
            .enumerate()
            .filter(move |&(bit, _)| self.0 & 1 << bit != 0)
            .map(|(_, name)| name)
    }

    /// The buttons held here and not in `other`.
    pub fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl fmt::Display for Buttons {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("-");
        }
        for (bit, letter) in Self::LETTERS.into_iter().enumerate() {
            if self.0 & 1 << bit != 0 {
                write!(f, "{letter}")?;
            }
        }
        Ok(())
    }
}

/// A parsed input file: the stops where the held buttons change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    changes: Vec<(u64, Buttons)>,
}

impl Input {
    /// Parses the text of an input file.
    ///
    /// # Errors
    /// The first line that is not `<stop> <buttons>`, or whose stop does not rise.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut changes: Vec<(u64, Buttons)> = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let content = line.split('#').next().unwrap_or_default().trim();
            if content.is_empty() {
                continue;
            }
            let invalid = |problem: &str| Error::Input {
                line: number + 1,
                problem: problem.to_owned(),
            };
            let mut words = content.split_whitespace();
            let (Some(stop), Some(buttons), None) = (words.next(), words.next(), words.next())
            else {
                return Err(invalid("expected `<stop> <buttons>`"));
            };
            let stop: u64 = stop
                .parse()
                .ok()
                .filter(|&stop| stop > 0)
                .ok_or_else(|| invalid("the stop must be a number from 1"))?;
            let buttons = Buttons::parse(buttons)
                .ok_or_else(|| invalid("buttons are distinct letters from UDLRABCS, or -"))?;
            if changes.last().is_some_and(|&(last, _)| stop <= last) {
                return Err(invalid("stops must rise from line to line"));
            }
            changes.push((stop, buttons));
        }
        Ok(Self { changes })
    }

    /// Reads and parses an input file.
    ///
    /// # Errors
    /// A file that cannot be read, or the first invalid line.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text)
    }

    /// The buttons held from `stop` on.
    pub fn at(&self, stop: u64) -> Buttons {
        self.changes
            .iter()
            .take_while(|&&(from, _)| from <= stop)
            .last()
            .map_or(Buttons(0), |&(_, buttons)| buttons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_parse_holds_buttons_until_the_next_change() {
        let input = Input::parse("# boot\n\n300 S  # press Start\n310 -\n400 RC\n").expect("ok");
        let held: Vec<String> = [1, 299, 300, 309, 310, 400, 9_999]
            .map(|stop| input.at(stop).to_string())
            .into();
        assert_eq!(held, ["-", "-", "S", "S", "-", "RC", "RC"]);
    }

    #[test]
    fn test_input_parse_empty_holds_nothing() {
        assert_eq!(Input::parse("").expect("ok").at(1), Buttons(0));
    }

    #[test]
    fn test_input_parse_bad_lines_name_their_line() {
        let cases = [
            ("1 S\n1 -\n", 2),
            ("0 S\n", 1),
            ("5 X\n", 1),
            ("5 SS\n", 1),
            ("5\n", 1),
            ("\n5 S extra\n", 2),
        ];
        for (text, line) in cases {
            let error = Input::parse(text).expect_err(text);
            assert!(
                matches!(error, Error::Input { line: at, .. } if at == line),
                "{text:?}"
            );
        }
    }

    #[test]
    fn test_buttons_bindings_follow_bit_order() {
        let buttons = Buttons::parse("SUA").expect("ok");
        assert_eq!(buttons.0, 0x91);
        assert_eq!(buttons.bindings().collect::<Vec<_>>(), ["up", "a", "start"]);
        assert_eq!(buttons.without(Buttons(0x01)).to_string(), "AS");
    }
}
