//! The low-frequency oscillator: one 7-bit counter for the whole chip, stepped at one of
//! eight rates, read as a triangle for amplitude modulation and as a signed 5-bit wave for
//! phase modulation.
//!
//! Sources: register $22, bit 3 on and bits 0-2 the rate, and the eight frequencies
//! (YM2608 manual 2-7, Plutiedev); a triangle, not a sine (Sauraen on SpritesMind, t=386
//! p=28164); the phase wave in 32 steps, negative in its second half (Eke, p=8909). No
//! document has the counter (side-session note v0.2-m3.5-research-fm.md, section 6), so it
//! is settled by ymfm [O]: 128 steps a period, read as a 64-step triangle for amplitude and
//! as eight levels mirrored and then negated for phase; the samples per step from the
//! manual's frequencies at the YM2608's 8 MHz clock, with one sample less once running and
//! two more for the first step; and a stopped LFO holding its counter at 0, which reads as
//! the deepest amplitude modulation (Eke, p=9005, relays Nemesis saying a stopped LFO holds
//! the level it stopped at instead [U]).

/// The manual's LFO frequencies in hundredths of a hertz.
const FREQUENCIES: [u32; 8] = [398, 556, 602, 637, 688, 963, 4810, 7220];

/// The YM2608 manual's sample rate the frequencies were given at: 8 MHz / 144, in
/// hundredths of a sample per second.
const MANUAL_RATE: u32 = 800_000_000 / 144;

/// Counter steps per LFO period.
const STEPS: u32 = 128;

/// For each rate, the sample count at which the counter steps: the manual's frequency as
/// samples per step, rounded.
const LIMITS: [u8; 8] = {
    let mut limits = [0; 8];
    let mut rate = 0;
    while rate < 8 {
        let per_period = STEPS * FREQUENCIES[rate];
        limits[rate] = ((MANUAL_RATE + per_period / 2) / per_period) as u8;
        rate += 1;
    }
    limits
};

/// Where the sample count starts again after a step.
const RESTART: u8 = 2;

/// Register $22's on bit.
const ON: u8 = 0x08;

/// The LFO's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Lfo {
    /// Samples since the last step.
    count: u8,
    /// The 7-bit position in the LFO's period.
    step: u8,
}

/// The LFO's output for one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Modulation {
    /// 6-bit amplitude modulation, as attenuation before the channel's sensitivity.
    pub amplitude: u8,
    /// Signed 5-bit phase modulation, -7 to 7 over a period.
    pub phase: i8,
}

impl Lfo {
    /// Moves the LFO on one sample, as register $22 is, and gives its output.
    pub fn clock(&mut self, register: u8) -> Modulation {
        if register & ON == 0 {
            *self = Self::default();
        } else if self.count >= LIMITS[usize::from(register & 0x07)] {
            self.step = (self.step + 1) & 0x7F;
            self.count = RESTART;
        } else {
            self.count += 1;
        }
        self.modulation()
    }

    fn modulation(&self) -> Modulation {
        let mut amplitude = self.step & 0x3F;
        if self.step & 0x40 == 0 {
            amplitude ^= 0x3F;
        }
        let mut phase = (self.step >> 2 & 0x07) as i8;
        if self.step & 0x20 != 0 {
            phase ^= 0x07;
        }
        if self.step & 0x40 != 0 {
            phase = -phase;
        }
        Modulation { amplitude, phase }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_come_from_the_manual_frequencies() {
        assert_eq!(LIMITS, [109, 78, 72, 68, 63, 45, 9, 6]);
    }

    #[test]
    fn test_stopped_lfo_reads_the_deepest_amplitude_and_no_phase() {
        let mut lfo = Lfo::default();
        let modulation = lfo.clock(0x07);
        assert_eq!(
            modulation,
            Modulation {
                amplitude: 0x3F,
                phase: 0
            }
        );
    }

    #[test]
    fn test_first_step_comes_after_limit_plus_one_samples_then_every_limit_minus_one() {
        let mut lfo = Lfo::default();
        let steps: Vec<u32> = (1..=200)
            .filter(|_| {
                let before = lfo.step;
                lfo.clock(ON | 0x07);
                lfo.step != before
            })
            .collect();
        assert_eq!(steps[..3], [7, 12, 17]);
    }

    #[test]
    fn test_amplitude_is_a_triangle_and_phase_a_signed_wave() {
        let lfo = |step| Lfo { count: 0, step }.modulation();
        assert_eq!(lfo(0).amplitude, 0x3F);
        assert_eq!(lfo(63).amplitude, 0);
        assert_eq!(lfo(64).amplitude, 0);
        assert_eq!(lfo(127).amplitude, 0x3F);
        assert_eq!(lfo(0).phase, 0);
        assert_eq!(lfo(28).phase, 7);
        assert_eq!(lfo(32).phase, 7);
        assert_eq!(lfo(63).phase, 0);
        assert_eq!(lfo(64 + 28).phase, -7);
    }
}
