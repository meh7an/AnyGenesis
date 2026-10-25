//! Comparing two runs stop by stop and reporting the first difference.

use std::{fmt, path::PathBuf};

use crate::{
    Error,
    dump::{self, Dump, Region},
    mask::Masks,
};

/// Differing addresses a report lists for one region, after the first.
const LISTED: usize = 7;

/// How far, in stops either way, a divergence is checked for a shifted match.
pub const SHIFT_WINDOW: u64 = 3;

/// A run's stops, numbered from 1.
pub trait Run {
    /// Stop `index`, or `None` past the run's last stop.
    ///
    /// # Errors
    /// A stop that exists but cannot be read.
    fn stop(&mut self, index: u64) -> Result<Option<Dump>, Error>;

    /// What the run noted about stop `index`, if anything.
    ///
    /// # Errors
    /// A note that exists but cannot be read.
    fn note(&mut self, _index: u64) -> Result<Option<String>, Error> {
        Ok(None)
    }
}

/// A run kept in a folder in the dump layout.
#[derive(Debug, Clone)]
pub struct Folder(pub PathBuf);

impl Run for Folder {
    fn stop(&mut self, index: u64) -> Result<Option<Dump>, Error> {
        Dump::read(&self.0, index)
    }

    fn note(&mut self, index: u64) -> Result<Option<String>, Error> {
        dump::read_note(&self.0, index)
    }
}

impl Run for Vec<Dump> {
    fn stop(&mut self, index: u64) -> Result<Option<Dump>, Error> {
        let Some(position) = index.checked_sub(1) else {
            return Ok(None);
        };
        Ok(usize::try_from(position)
            .ok()
            .and_then(|position| self.get(position))
            .cloned())
    }
}

/// The unmasked bytes of one region that differ at one stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionDifference {
    pub region: Region,
    /// Differing addresses, ascending.
    pub addresses: Vec<u32>,
    /// Our byte and the reference's at the first differing address.
    pub first: (u8, u8),
}

/// Every region in which `ours` and `reference` differ outside `masks`.
pub fn compare(ours: &Dump, reference: &Dump, masks: &Masks) -> Vec<RegionDifference> {
    Region::ALL
        .into_iter()
        .filter_map(|region| {
            let (mine, theirs) = (ours.region(region), reference.region(region));
            let offsets: Vec<usize> = (0..region.size())
                .filter(|&offset| mine[offset] != theirs[offset])
                .filter(|&offset| !masks.covers(region, offset))
                .collect();
            let &first = offsets.first()?;
            Some(RegionDifference {
                region,
                // Regions are at most 64 KiB, so offsets fit next to the base.
                addresses: offsets
                    .iter()
                    .map(|&offset| region.base() + offset as u32)
                    .collect(),
                first: (mine[first], theirs[first]),
            })
        })
        .collect()
}

/// The first stop at which the runs differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub stop: u64,
    pub regions: Vec<RegionDifference>,
    /// `d` when the reference's stop equals our stop `stop + d` outside the masks: the runs
    /// agree but drifted apart in time.
    pub shift: Option<i64>,
    /// What our run noted at this stop.
    pub note: Option<String>,
}

/// The result of diffing two runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Stops compared, from 1, before the first divergence or the end of either run.
    pub matched: u64,
    pub divergence: Option<Divergence>,
    /// When no divergence was found: which runs ended after `matched` stops.
    pub ours_ended: bool,
    pub reference_ended: bool,
}

/// Compares `ours` with `reference` from stop 1 until they first differ outside `masks` or
/// either run ends.
///
/// # Errors
/// A stop or note that cannot be read.
pub fn diff(ours: &mut dyn Run, reference: &mut dyn Run, masks: &Masks) -> Result<Report, Error> {
    let mut matched = 0;
    loop {
        let index = matched + 1;
        let (Some(mine), Some(theirs)) = (ours.stop(index)?, reference.stop(index)?) else {
            return Ok(Report {
                matched,
                divergence: None,
                ours_ended: ours.stop(index)?.is_none(),
                reference_ended: reference.stop(index)?.is_none(),
            });
        };
        let regions = compare(&mine, &theirs, masks);
        if regions.is_empty() {
            matched = index;
            continue;
        }
        let divergence = Divergence {
            stop: index,
            regions,
            shift: shift(ours, &theirs, index, masks)?,
            note: ours.note(index)?,
        };
        return Ok(Report {
            matched,
            divergence: Some(divergence),
            ours_ended: false,
            reference_ended: false,
        });
    }
}

/// The nearest offset `d` within [`SHIFT_WINDOW`], earlier first on a tie, for which our
/// stop `index + d` equals `theirs` outside `masks`.
fn shift(
    ours: &mut dyn Run,
    theirs: &Dump,
    index: u64,
    masks: &Masks,
) -> Result<Option<i64>, Error> {
    for distance in 1..=SHIFT_WINDOW {
        let candidates = [index.checked_sub(distance), index.checked_add(distance)];
        for (candidate, sign) in candidates.into_iter().zip([-1, 1]) {
            let Some(candidate) = candidate.filter(|&candidate| candidate > 0) else {
                continue;
            };
            if let Some(mine) = ours.stop(candidate)?
                && compare(&mine, theirs, masks).is_empty()
            {
                return Ok(Some(sign * distance as i64));
            }
        }
    }
    Ok(None)
}

impl fmt::Display for RegionDifference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mine, theirs) = self.first;
        let width = if self.region == Region::Ram { 6 } else { 4 };
        write!(
            f,
            "{}: {} bytes differ, first at ${:0width$X} (ours ${mine:02X}, reference \
             ${theirs:02X})",
            self.region,
            self.addresses.len(),
            self.addresses[0],
        )?;
        let more: Vec<String> = self.addresses[1..]
            .iter()
            .take(LISTED)
            .map(|address| format!("${address:0width$X}"))
            .collect();
        if !more.is_empty() {
            write!(f, "; then {}", more.join(", "))?;
            if self.addresses.len() > LISTED + 1 {
                f.write_str(", ...")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(divergence) = &self.divergence else {
            writeln!(f, "no differences in {} stops", self.matched)?;
            let ended = match (self.ours_ended, self.reference_ended) {
                (true, true) => "both runs end there",
                (true, false) => "our run ends there; the reference has more stops",
                (false, true) => "the reference ends there; our run has more stops",
                (false, false) => "",
            };
            return writeln!(f, "{ended}");
        };
        writeln!(
            f,
            "first divergence at stop {} (stops 1-{} match)",
            divergence.stop, self.matched
        )?;
        for region in &divergence.regions {
            writeln!(f, "  {region}")?;
        }
        match divergence.shift {
            Some(shift) => writeln!(
                f,
                "  the reference's stop {} equals our stop {} (a shift of {shift:+}: the runs \
                 drifted apart in time, which is still a divergence)",
                divergence.stop,
                divergence.stop.saturating_add_signed(shift)
            )?,
            None => writeln!(
                f,
                "  no stop of ours within {SHIFT_WINDOW} either way equals the reference's"
            )?,
        }
        if let Some(note) = &divergence.note {
            writeln!(f, "  our run at this stop:")?;
            for line in note.lines() {
                writeln!(f, "    {line}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run of `count` stops whose RAM byte 0 counts up from 1.
    fn counting(count: u8) -> Vec<Dump> {
        (1..=count)
            .map(|value| {
                let mut dump = Dump::default();
                dump.region_mut(Region::Ram)[0] = value;
                dump
            })
            .collect()
    }

    fn run(ours: &mut Vec<Dump>, reference: &mut Vec<Dump>, masks: &Masks) -> Report {
        diff(ours, reference, masks).expect("in memory")
    }

    #[test]
    fn test_diff_identical_runs_report_no_divergence() {
        let report = run(&mut counting(5), &mut counting(5), &Masks::default());
        assert_eq!((report.matched, &report.divergence), (5, &None));
        assert!(report.ours_ended && report.reference_ended);
        assert!(report.to_string().contains("no differences in 5 stops"));
    }

    #[test]
    fn test_diff_shorter_reference_compares_the_common_stops() {
        let report = run(&mut counting(5), &mut counting(3), &Masks::default());
        assert_eq!(report.matched, 3);
        assert!(!report.ours_ended && report.reference_ended);
    }

    #[test]
    fn test_diff_reports_the_first_differing_stop_region_and_address() {
        let mut reference = counting(6);
        reference[3].region_mut(Region::Vram)[0x1234] = 0x55;
        reference[3].region_mut(Region::Vram)[0x1236] = 0x66;
        reference[4].region_mut(Region::Cram)[0] = 0x0E;
        let report = run(&mut counting(6), &mut reference, &Masks::default());
        let divergence = report.divergence.clone().expect("diverges");
        assert_eq!((report.matched, divergence.stop), (3, 4));
        assert_eq!(divergence.regions.len(), 1);
        let region = &divergence.regions[0];
        assert_eq!(region.region, Region::Vram);
        assert_eq!(region.addresses, [0x1234, 0x1236]);
        assert_eq!(region.first, (0x00, 0x55));
        assert_eq!(divergence.shift, None);
        let text = report.to_string();
        assert!(text.contains("first divergence at stop 4"), "{text}");
        let line = "VRAM: 2 bytes differ, first at $1234 (ours $00, reference $55); then $1236";
        assert!(text.contains(line), "{text}");
    }

    #[test]
    fn test_diff_ram_addresses_are_68000_addresses() {
        let mut reference = counting(2);
        reference[0].region_mut(Region::Ram)[0xF600] = 4;
        let report = run(&mut counting(2), &mut reference, &Masks::default());
        let divergence = report.divergence.expect("diverges");
        assert_eq!(divergence.regions[0].addresses, [0xFF_F600]);
    }

    #[test]
    fn test_diff_masked_bytes_are_ignored() {
        let mut reference = counting(3);
        reference[1].region_mut(Region::Ram)[0xFDEA] = 0x12;
        let text = concat!(
            "[[mask]]\nregion = \"ram\"\nstart = 0xFFFDE8\nend = 0xFFFDF0\n",
            "reason = \"stale frames\"\n"
        );
        let masks = Masks::parse(text).expect("ok");
        let report = run(&mut counting(3), &mut reference, &masks);
        assert_eq!((report.matched, report.divergence), (3, None));
    }

    #[test]
    fn test_diff_a_delayed_reference_is_reported_as_a_shift() {
        // The reference repeats stop 2, so from stop 3 on it is one stop behind ours.
        let mut reference = counting(6);
        reference.insert(2, reference[1].clone());
        let report = run(&mut counting(6), &mut reference, &Masks::default());
        let divergence = report.divergence.expect("diverges");
        assert_eq!((divergence.stop, divergence.shift), (3, Some(-1)));
    }

    #[test]
    fn test_diff_note_of_our_stop_is_reported() {
        struct Noting(Vec<Dump>);
        impl Run for Noting {
            fn stop(&mut self, index: u64) -> Result<Option<Dump>, Error> {
                self.0.stop(index)
            }
            fn note(&mut self, index: u64) -> Result<Option<String>, Error> {
                Ok(Some(format!("pc of stop {index}\n")))
            }
        }
        let mut reference = counting(2);
        reference[1].region_mut(Region::Registers)[1] = 0x74;
        let report = diff(&mut Noting(counting(2)), &mut reference, &Masks::default()).expect("ok");
        assert!(report.to_string().contains("    pc of stop 2"));
    }

    #[test]
    fn test_region_difference_lists_at_most_eight_addresses() {
        let difference = RegionDifference {
            region: Region::Vram,
            addresses: (0..20).collect(),
            first: (1, 2),
        };
        let text = difference.to_string();
        assert!(text.ends_with("$0007, ..."), "{text}");
    }
}
