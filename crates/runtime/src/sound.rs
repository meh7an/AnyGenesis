//! The writes that reach the sound chips, from either CPU: counted always, kept in order
//! when a run asks for them. There is no sound output yet; the PSG and the YM2612's
//! synthesis come later, and these writes are what they will play.

/// The CPU that made a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cpu {
    M68000,
    Z80,
}

impl Cpu {
    fn index(self) -> usize {
        match self {
            Self::M68000 => 0,
            Self::Z80 => 1,
        }
    }
}

/// The chip a write reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    /// The YM2612's register `register` of part `part` (1 or 2), through its data port.
    Ym2612 { part: u8, register: u8 },
    /// The PSG's one write port.
    Psg,
}

/// The YM2612 register whose data writes play the DAC channel.
pub const DAC_DATA: u8 = 0x2A;

/// One write to a sound chip at master clock `clock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundWrite {
    pub clock: u64,
    pub cpu: Cpu,
    pub chip: Chip,
    pub value: u8,
}

impl SoundWrite {
    /// Whether this is a DAC sample: data for YM2612 register $2A of part 1.
    pub fn is_dac(&self) -> bool {
        self.chip
            == Chip::Ym2612 {
                part: 1,
                register: DAC_DATA,
            }
    }
}

/// Writes counted per CPU (index 0 the 68000, 1 the Z80).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SoundCounts {
    /// YM2612 data writes, DAC samples included.
    pub ym2612: [u64; 2],
    /// DAC samples.
    pub dac: [u64; 2],
    pub psg: [u64; 2],
    /// YM2612 timer loads or enables, which start no timer yet.
    pub timers: [u64; 2],
}

impl SoundCounts {
    fn count(&mut self, write: &SoundWrite) {
        let cpu = write.cpu.index();
        match write.chip {
            Chip::Ym2612 { .. } => {
                self.ym2612[cpu] += 1;
                if write.is_dac() {
                    self.dac[cpu] += 1;
                }
            }
            Chip::Psg => self.psg[cpu] += 1,
        }
    }

    /// The counts of both, added.
    pub fn sum(&self, other: &Self) -> Self {
        let add = |a: [u64; 2], b: [u64; 2]| [a[0] + b[0], a[1] + b[1]];
        Self {
            ym2612: add(self.ym2612, other.ym2612),
            dac: add(self.dac, other.dac),
            psg: add(self.psg, other.psg),
            timers: add(self.timers, other.timers),
        }
    }
}

/// Sound writes of one source, counted, and kept while recording is on. Positions are
/// counted from the first write ever kept, so a caller can cut back to one after older
/// writes have been taken.
#[derive(Debug, Default, Clone)]
pub(crate) struct SoundLog {
    counts: SoundCounts,
    writes: Vec<SoundWrite>,
    /// Writes taken out so far: the position of `writes[0]`.
    taken: usize,
    recording: bool,
}

impl SoundLog {
    pub fn record(&mut self, write: SoundWrite) {
        self.counts.count(&write);
        if self.recording {
            self.writes.push(write);
        }
    }

    /// Counts a YM2612 timer load or enable by `cpu`.
    pub fn timer(&mut self, cpu: Cpu) {
        self.counts.timers[cpu.index()] += 1;
    }

    pub fn counts(&self) -> SoundCounts {
        self.counts
    }

    pub fn set_recording(&mut self, recording: bool) {
        self.recording = recording;
    }

    /// Where the next write will be kept, to cut back to with [`SoundLog::truncate`].
    pub fn position(&self) -> usize {
        self.taken + self.writes.len()
    }

    /// Forgets the writes from `position` on and puts the counts back to `counts`.
    pub fn truncate(&mut self, position: usize, counts: SoundCounts) {
        self.writes.truncate(position.saturating_sub(self.taken));
        self.counts = counts;
    }

    /// Takes the kept writes made before master clock `until`.
    pub fn take_before(&mut self, until: u64) -> Vec<SoundWrite> {
        let count = self.writes.partition_point(|write| write.clock < until);
        self.taken += count;
        self.writes.drain(..count).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ym(cpu: Cpu, register: u8, clock: u64) -> SoundWrite {
        SoundWrite {
            clock,
            cpu,
            chip: Chip::Ym2612 { part: 1, register },
            value: 0x80,
        }
    }

    #[test]
    fn test_counts_tell_dac_samples_and_cpus_apart() {
        let mut log = SoundLog::default();
        log.record(ym(Cpu::Z80, DAC_DATA, 1));
        log.record(ym(Cpu::Z80, 0x28, 2));
        log.record(ym(Cpu::M68000, DAC_DATA, 3));
        log.record(SoundWrite {
            clock: 4,
            cpu: Cpu::M68000,
            chip: Chip::Psg,
            value: 0x9F,
        });
        let counts = log.counts();
        assert_eq!(counts.ym2612, [1, 2]);
        assert_eq!(counts.dac, [1, 1]);
        assert_eq!(counts.psg, [1, 0]);
        assert!(log.take_before(u64::MAX).is_empty(), "not recording");
    }

    #[test]
    fn test_truncate_and_take_keep_positions_across_takes() {
        let mut log = SoundLog::default();
        log.set_recording(true);
        log.record(ym(Cpu::Z80, DAC_DATA, 10));
        log.record(ym(Cpu::Z80, DAC_DATA, 20));
        assert_eq!(log.take_before(15).len(), 1);
        let position = log.position();
        let counts = log.counts();
        assert_eq!(position, 2);
        log.record(ym(Cpu::Z80, DAC_DATA, 30));
        log.truncate(position, counts);
        assert_eq!(log.counts().dac, [0, 2]);
        let rest = log.take_before(u64::MAX);
        assert_eq!(
            rest.iter().map(|write| write.clock).collect::<Vec<_>>(),
            [20]
        );
    }
}
