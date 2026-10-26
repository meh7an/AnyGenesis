//! Comparing two runs stop by stop, realigning on whole-stop shifts.
//!
//! Our stop `k + offset` pairs with the reference's stop `k`, the offset starting at 0. Where
//! a pair differs, the diff looks for another offset within [`SHIFT_WINDOW`] at which that
//! stop and the [`SHIFT_CONFIRMATION`] after it all match: the runs agree but one took a
//! vertical interrupt more than the other. Such a shift is taken and reported; a pair that
//! differs under every allowed offset is a divergence.

use std::{fmt, path::PathBuf};

use crate::{
    Error,
    dump::{self, Dump, Region},
    mask::Masks,
};

/// Differing addresses a report lists for one region, after the first.
const LISTED: usize = 7;

/// Differing addresses a one-line summary lists for one region.
const BRIEF: usize = 4;

/// How far, in stops either way, our run may drift from the reference's.
pub const SHIFT_WINDOW: i64 = 3;

/// Stops after a shift's first that must match too before the shift is taken.
pub const SHIFT_CONFIRMATION: u64 = 2;

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

/// A reference stop that differs from ours under every allowed offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The reference's stop.
    pub stop: u64,
    /// Our stop paired with it at the offset in force.
    pub ours: u64,
    pub regions: Vec<RegionDifference>,
    /// What our run noted at our stop.
    pub note: Option<String>,
    /// The pairing within [`SHIFT_WINDOW`] at which the reference's stop differs least.
    pub closest: Option<Closest>,
}

/// Another pairing for a divergent stop: our stop `ours`, at `offset`, and what differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closest {
    pub offset: i64,
    pub ours: u64,
    pub regions: Vec<RegionDifference>,
}

impl Divergence {
    /// The closest pairing, when it is not the one in force.
    fn other_closest(&self) -> Option<&Closest> {
        self.closest
            .as_ref()
            .filter(|closest| closest.ours != self.ours)
    }
}

impl Closest {
    fn bytes(&self) -> usize {
        self.regions
            .iter()
            .map(|region| region.addresses.len())
            .sum()
    }
}

/// A change of pairing: from the reference's stop `stop` on, our stop `stop + to` pairs with
/// it instead of `stop + from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shift {
    pub stop: u64,
    pub from: i64,
    pub to: i64,
}

/// The result of diffing two runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The reference's stops that had a partner, from 1.
    pub compared: u64,
    /// Every realignment, in order.
    pub shifts: Vec<Shift>,
    /// Every pair that differs outside the masks under every allowed offset, in order.
    pub divergences: Vec<Divergence>,
    /// Which runs ran out of stops after `compared`.
    pub ours_ended: bool,
    pub reference_ended: bool,
}

/// Our stop paired with the reference's stop `stop` at `offset`, if there is one.
fn partner(stop: u64, offset: i64) -> Option<u64> {
    stop.checked_add_signed(offset).filter(|&index| index > 0)
}

/// Compares `ours` with `reference` until either run ends, realigning on whole-stop shifts
/// and listing every stop that differs outside `masks` under every allowed offset.
///
/// # Errors
/// A stop or note that cannot be read.
pub fn diff(ours: &mut dyn Run, reference: &mut dyn Run, masks: &Masks) -> Result<Report, Error> {
    let mut report = Report::default();
    let mut offset = 0;
    for stop in 1.. {
        let index = partner(stop, offset);
        let mine = index.map(|index| ours.stop(index)).transpose()?.flatten();
        let theirs = reference.stop(stop)?;
        let (Some(mine), Some(theirs), Some(index)) = (&mine, &theirs, index) else {
            report.ours_ended = mine.is_none();
            report.reference_ended = theirs.is_none();
            break;
        };
        report.compared = stop;
        let regions = compare(mine, theirs, masks);
        if regions.is_empty() {
            continue;
        }
        if let Some(to) = realign(ours, reference, stop, offset, masks)? {
            report.shifts.push(Shift {
                stop,
                from: offset,
                to,
            });
            offset = to;
            continue;
        }
        report.divergences.push(Divergence {
            stop,
            ours: index,
            regions,
            note: ours.note(index)?,
            closest: closest(ours, theirs, stop, masks)?,
        });
    }
    Ok(report)
}

/// The offset within [`SHIFT_WINDOW`] at which our partner of `theirs`, the reference's stop
/// `stop`, differs in the fewest bytes; the nearest offset, earlier first, on a tie.
fn closest(
    ours: &mut dyn Run,
    theirs: &Dump,
    stop: u64,
    masks: &Masks,
) -> Result<Option<Closest>, Error> {
    let mut best: Option<Closest> = None;
    let mut offsets: Vec<i64> = (-SHIFT_WINDOW..=SHIFT_WINDOW).collect();
    offsets.sort_by_key(|offset| offset.abs());
    for offset in offsets {
        let Some(index) = partner(stop, offset) else {
            continue;
        };
        let Some(mine) = ours.stop(index)? else {
            continue;
        };
        let candidate = Closest {
            offset,
            ours: index,
            regions: compare(&mine, theirs, masks),
        };
        if best
            .as_ref()
            .is_none_or(|best| candidate.bytes() < best.bytes())
        {
            best = Some(candidate);
        }
    }
    Ok(best)
}

/// The offset within [`SHIFT_WINDOW`] other than `offset`, nearest to it and earlier first on
/// a tie, at which the pairs from the reference's stop `stop` on match.
fn realign(
    ours: &mut dyn Run,
    reference: &mut dyn Run,
    stop: u64,
    offset: i64,
    masks: &Masks,
) -> Result<Option<i64>, Error> {
    for distance in 1..=2 * SHIFT_WINDOW {
        for to in [offset - distance, offset + distance] {
            if to.abs() <= SHIFT_WINDOW && pairs_match(ours, reference, stop, to, masks)? {
                return Ok(Some(to));
            }
        }
    }
    Ok(None)
}

/// Whether, at `offset`, the reference's stop `stop` and the [`SHIFT_CONFIRMATION`] after it
/// all have partners equal to them outside `masks`.
fn pairs_match(
    ours: &mut dyn Run,
    reference: &mut dyn Run,
    stop: u64,
    offset: i64,
    masks: &Masks,
) -> Result<bool, Error> {
    for stop in stop..=stop + SHIFT_CONFIRMATION {
        let Some(index) = partner(stop, offset) else {
            return Ok(false);
        };
        let (Some(mine), Some(theirs)) = (ours.stop(index)?, reference.stop(stop)?) else {
            return Ok(false);
        };
        if !compare(&mine, &theirs, masks).is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

impl RegionDifference {
    /// Hex digits an address of the region is shown with.
    fn width(&self) -> usize {
        if self.region == Region::Ram { 6 } else { 4 }
    }
}

/// Each region's differing byte count with its first few addresses, or "equal".
fn brief(regions: &[RegionDifference]) -> String {
    if regions.is_empty() {
        return "equal".to_owned();
    }
    let parts: Vec<String> = regions
        .iter()
        .map(|region| {
            let width = region.width();
            let shown: Vec<String> = region
                .addresses
                .iter()
                .take(BRIEF)
                .map(|address| format!("${address:0width$X}"))
                .collect();
            let more = if region.addresses.len() > BRIEF {
                ", ..."
            } else {
                ""
            };
            format!(
                "{} {} ({}{more})",
                region.region,
                region.addresses.len(),
                shown.join(", ")
            )
        })
        .collect();
    parts.join(", ")
}

impl fmt::Display for Closest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "closest at offset {:+} (our stop {}): {}",
            self.offset,
            self.ours,
            brief(&self.regions)
        )
    }
}

impl fmt::Display for RegionDifference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mine, theirs) = self.first;
        let width = self.width();
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

/// "stop 5" or "stops 5-7".
fn stops(first: u64, last: u64) -> String {
    if first == last {
        format!("stop {first}")
    } else {
        format!("stops {first}-{last}")
    }
}

impl fmt::Display for Shift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let from = self.stop.saturating_add_signed(self.from);
        let to = self.stop.saturating_add_signed(self.to);
        write!(
            f,
            "from reference stop {}: our stop {to} (offset {:+} to {:+}); ",
            self.stop, self.from, self.to
        )?;
        if to > from {
            write!(
                f,
                "no reference stop pairs with our {}",
                stops(from, to - 1)
            )
        } else {
            write!(
                f,
                "the reference pairs twice with our {}",
                stops(to, from - 1)
            )
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "compared reference stops 1-{}, {} whole-stop shifts (within {SHIFT_WINDOW}, {} \
             more stops confirming each)",
            self.compared,
            self.shifts.len(),
            SHIFT_CONFIRMATION
        )?;
        for shift in &self.shifts {
            writeln!(f, "  shift {shift}")?;
        }
        let ended = match (self.ours_ended, self.reference_ended) {
            (true, true) => "both runs end there",
            (true, false) => "our run ends there; the reference has more stops",
            (false, true) => "the reference ends there; our run has more stops",
            (false, false) => "",
        };
        writeln!(f, "{ended}")?;
        let Some(first) = self.divergences.first() else {
            return writeln!(f, "no differences in {} stops", self.compared);
        };
        writeln!(
            f,
            "{} divergent stops; first at reference stop {} (our stop {})",
            self.divergences.len(),
            first.stop,
            first.ours
        )?;
        for region in &first.regions {
            writeln!(f, "  {region}")?;
        }
        if let Some(closest) = first.other_closest() {
            writeln!(f, "  {closest}")?;
        }
        if let Some(note) = &first.note {
            writeln!(f, "  our run at this stop:")?;
            for line in note.lines() {
                writeln!(f, "    {line}")?;
            }
        }
        for divergence in &self.divergences[1..] {
            write!(
                f,
                "  reference stop {} (our stop {}): {}",
                divergence.stop,
                divergence.ours,
                brief(&divergence.regions)
            )?;
            match divergence.other_closest() {
                Some(closest) => writeln!(f, "; {closest}")?,
                None => writeln!(f)?,
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
        assert_eq!(report.compared, 5);
        assert!(report.shifts.is_empty() && report.divergences.is_empty());
        assert!(report.ours_ended && report.reference_ended);
        assert!(report.to_string().contains("no differences in 5 stops"));
    }

    #[test]
    fn test_diff_shorter_reference_compares_the_common_stops() {
        let report = run(&mut counting(5), &mut counting(3), &Masks::default());
        assert_eq!(report.compared, 3);
        assert!(!report.ours_ended && report.reference_ended);
    }

    #[test]
    fn test_diff_reports_every_differing_stop_region_and_address() {
        let mut reference = counting(6);
        reference[3].region_mut(Region::Vram)[0x1234] = 0x55;
        reference[3].region_mut(Region::Vram)[0x1236] = 0x66;
        reference[4].region_mut(Region::Cram)[0] = 0x0E;
        let report = run(&mut counting(6), &mut reference, &Masks::default());
        let stops: Vec<(u64, u64)> = report
            .divergences
            .iter()
            .map(|divergence| (divergence.stop, divergence.ours))
            .collect();
        assert_eq!(stops, [(4, 4), (5, 5)]);
        assert!(report.shifts.is_empty());
        let first = &report.divergences[0];
        assert_eq!(first.regions.len(), 1);
        let region = &first.regions[0];
        assert_eq!(region.region, Region::Vram);
        assert_eq!(region.addresses, [0x1234, 0x1236]);
        assert_eq!(region.first, (0x00, 0x55));
        let text = report.to_string();
        assert!(text.contains("2 divergent stops; first at reference stop 4 (our stop 4)"));
        let line = "VRAM: 2 bytes differ, first at $1234 (ours $00, reference $55); then $1236";
        assert!(text.contains(line), "{text}");
        let line = "reference stop 5 (our stop 5): CRAM 1 ($0000)\n";
        assert!(text.contains(line), "{text}");
    }

    #[test]
    fn test_diff_ram_addresses_are_68000_addresses() {
        let mut reference = counting(2);
        reference[0].region_mut(Region::Ram)[0xF600] = 4;
        let report = run(&mut counting(2), &mut reference, &Masks::default());
        assert_eq!(report.divergences[0].regions[0].addresses, [0xFF_F600]);
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
        assert_eq!(report.compared, 3);
        assert!(report.divergences.is_empty());
    }

    #[test]
    fn test_diff_a_delayed_reference_realigns_with_a_reported_shift() {
        // The reference repeats stop 2, so from stop 3 on it is one stop behind ours.
        let mut reference = counting(6);
        reference.insert(2, reference[1].clone());
        let report = run(&mut counting(6), &mut reference, &Masks::default());
        let shift = Shift {
            stop: 3,
            from: 0,
            to: -1,
        };
        assert_eq!(report.shifts, [shift]);
        assert!(report.divergences.is_empty());
        assert_eq!(report.compared, 7);
        assert!(report.ours_ended && report.reference_ended);
        let text = report.to_string();
        let line = "shift from reference stop 3: our stop 2 (offset +0 to -1); the reference \
                    pairs twice with our stop 2";
        assert!(text.contains(line), "{text}");
        assert!(text.contains("no differences in 7 stops"), "{text}");
    }

    #[test]
    fn test_diff_an_extra_stop_of_ours_realigns_with_a_reported_shift() {
        let mut ours = counting(6);
        ours.insert(2, ours[1].clone());
        let report = run(&mut ours, &mut counting(6), &Masks::default());
        let shift = Shift {
            stop: 3,
            from: 0,
            to: 1,
        };
        assert_eq!(report.shifts, [shift]);
        assert!(report.divergences.is_empty());
        let text = report.to_string();
        assert!(
            text.contains("no reference stop pairs with our stop 3"),
            "{text}"
        );
    }

    #[test]
    fn test_diff_a_shift_past_the_window_is_a_divergence() {
        let mut ours = counting(12);
        for _ in 0..4 {
            ours.insert(2, ours[1].clone());
        }
        let report = run(&mut ours, &mut counting(12), &Masks::default());
        assert!(report.shifts.is_empty());
        assert_eq!(report.divergences[0].stop, 3);
    }

    #[test]
    fn test_diff_an_unconfirmed_shift_is_a_divergence() {
        // Reference stop 3 equals our stop 2, but stop 4 is back in step.
        let mut reference = counting(6);
        reference[2] = reference[1].clone();
        let report = run(&mut counting(6), &mut reference, &Masks::default());
        assert!(report.shifts.is_empty());
        let stops: Vec<u64> = report
            .divergences
            .iter()
            .map(|divergence| divergence.stop)
            .collect();
        assert_eq!(stops, [3]);
        let closest = report.divergences[0]
            .closest
            .clone()
            .expect("a closest pairing");
        assert_eq!((closest.offset, closest.ours), (-1, 2));
        assert!(closest.regions.is_empty());
        let text = report.to_string();
        assert!(
            text.contains("closest at offset -1 (our stop 2): equal"),
            "{text}"
        );
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
