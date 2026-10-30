use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use crate::{Address, ROM_MARKER};

/// What one run of a game executed in its ROM: how often each instruction ran and where its
/// computed jumps and calls went.
///
/// A build's profile run records one on the interpreter; the analysis follows its computed
/// targets, and tests measure from its counts how much of a run optimised code covers. As
/// text ([`Profile::text`]) it keeps the addresses, not the counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Instructions started at each even ROM address, indexed by address / 2.
    executed: Vec<u64>,
    /// Addresses with a count above zero.
    distinct: usize,
    /// Every JMP or JSR through a register or table that ran, with the targets it took.
    computed: BTreeMap<Address, BTreeSet<Address>>,
}

impl Profile {
    /// An empty profile for a ROM of `rom_size` bytes.
    pub fn new(rom_size: u32) -> Self {
        Self {
            executed: vec![0; rom_size.div_ceil(2) as usize],
            distinct: 0,
            computed: BTreeMap::new(),
        }
    }

    /// Counts an instruction started at `pc`; addresses outside the ROM (RAM code) and odd
    /// ones are not kept.
    pub fn instruction(&mut self, pc: u32) {
        if !pc.is_multiple_of(2) {
            return;
        }
        let Some(count) = self.executed.get_mut(pc as usize / 2) else {
            return;
        };
        if *count == 0 {
            self.distinct += 1;
        }
        *count += 1;
    }

    /// Notes that the computed jump or call at `site` went to `target`; RAM sites and
    /// targets are not kept.
    pub fn computed(&mut self, site: u32, target: u32) {
        if self.in_rom(site) && self.in_rom(target) {
            self.computed
                .entry(Address::new(site))
                .or_default()
                .insert(Address::new(target));
        }
    }

    /// How often the instruction at `address` started.
    pub fn count(&self, address: Address) -> u64 {
        let index = address.get() as usize / 2;
        if address.is_even() {
            self.executed.get(index).copied().unwrap_or(0)
        } else {
            0
        }
    }

    /// The instructions started in the ROM, counting repeats.
    pub fn total(&self) -> u64 {
        self.executed.iter().sum()
    }

    /// How many ROM addresses started an instruction at least once.
    pub fn distinct(&self) -> usize {
        self.distinct
    }

    /// The ROM addresses that started an instruction, ascending, with their counts.
    pub fn executed(&self) -> impl Iterator<Item = (Address, u64)> + '_ {
        self.executed
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count > 0)
            .map(|(index, &count)| (Address::new(index as u32 * 2), count))
    }

    /// The computed jumps and calls that ran, by site, with the targets each took.
    pub fn computed_targets(&self) -> &BTreeMap<Address, BTreeSet<Address>> {
        &self.computed
    }

    /// The profile as text, after the ROM marker line and the `frames` the run lasted: a line
    /// `run <address>` per address run, then `computed <site> <target>...` per computed
    /// jump or call.
    pub fn text(&self, frames: u64) -> String {
        let mut text = format!("# {ROM_MARKER}\nframes {frames}\n");
        for (address, _) in self.executed() {
            writeln!(text, "run {address}").expect("writing to a String");
        }
        for (site, targets) in &self.computed {
            write!(text, "computed {site}").expect("writing to a String");
            for target in targets {
                write!(text, " {target}").expect("writing to a String");
            }
            text.push('\n');
        }
        text
    }

    /// Reads [`Profile::text`] back for a ROM of `rom_size` bytes, with every address run
    /// counted once, and the frames the run lasted.
    ///
    /// # Errors
    /// The first line that is none of the above, by number from 1.
    pub fn parse(text: &str, rom_size: u32) -> Result<(Self, u64), ProfileError> {
        let mut profile = Self::new(rom_size);
        let mut frames = 0;
        for (index, line) in text.lines().enumerate() {
            let bad = ProfileError { line: index + 1 };
            let mut words = line.split_whitespace();
            match words.next() {
                None => {}
                Some(word) if word.starts_with('#') => {}
                Some("frames") => {
                    frames = words.next().and_then(|n| n.parse().ok()).ok_or(bad)?;
                }
                Some("run") => profile.instruction(words.next().and_then(hex).ok_or(bad)?),
                Some("computed") => {
                    let site = words.next().and_then(hex).ok_or(bad)?;
                    for word in words {
                        profile.computed(site, hex(word).ok_or(bad)?);
                    }
                }
                Some(_) => return Err(bad),
            }
        }
        Ok((profile, frames))
    }

    fn in_rom(&self, address: u32) -> bool {
        (address as usize / 2) < self.executed.len()
    }
}

/// A line of a profile's text that [`Profile::parse`] cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("line {line} of the profile is not a profile line")]
pub struct ProfileError {
    pub line: usize,
}

/// An address as [`Address`] prints it, `0x` and hex digits.
fn hex(word: &str) -> Option<u32> {
    u32::from_str_radix(word.strip_prefix("0x")?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_counts_rom_instructions_and_skips_ram_and_odd_ones() {
        let mut profile = Profile::new(0x100);
        profile.instruction(0x10);
        profile.instruction(0x10);
        profile.instruction(0x20);
        profile.instruction(0x11);
        profile.instruction(0xFF_0000);
        assert_eq!(profile.count(Address::new(0x10)), 2);
        assert_eq!(profile.count(Address::new(0x11)), 0);
        assert_eq!(profile.total(), 3);
        assert_eq!(profile.distinct(), 2);
        assert_eq!(
            profile.executed().collect::<Vec<_>>(),
            [(Address::new(0x10), 2), (Address::new(0x20), 1)]
        );
    }

    #[test]
    fn test_profile_keeps_computed_targets_inside_the_rom_only() {
        let mut profile = Profile::new(0x100);
        profile.computed(0x40, 0x80);
        profile.computed(0x40, 0x90);
        profile.computed(0x40, 0x80);
        profile.computed(0x42, 0xFF_8000);
        let targets = profile.computed_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[&Address::new(0x40)]
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [Address::new(0x80), Address::new(0x90)]
        );
    }

    #[test]
    fn test_profile_text_lists_addresses_and_parses_back() {
        let mut profile = Profile::new(0x100);
        profile.instruction(0x20);
        profile.instruction(0x10);
        profile.computed(0x20, 0x40);
        profile.computed(0x20, 0x30);
        let text = profile.text(7);
        assert_eq!(
            text,
            format!(
                "# {ROM_MARKER}\nframes 7\nrun 0x000010\nrun 0x000020\n\
                 computed 0x000020 0x000030 0x000040\n"
            )
        );
        assert_eq!(Profile::parse(&text, 0x100), Ok((profile, 7)));
        assert_eq!(
            Profile::parse("frames 1\nrun 20\n", 0x100),
            Err(ProfileError { line: 2 })
        );
    }
}
