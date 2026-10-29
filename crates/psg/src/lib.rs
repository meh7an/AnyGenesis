//! The PSG of the Genesis: the SN76489-compatible chip inside Sega's VDP, three square-wave
//! tone channels and one noise channel, each with its own attenuation.
//!
//! Written from documentation, no other emulator's code. Sources, cited where used (side-
//! session note v0.2-m3.3-research-psg.md has the quotes; [U] marks what came second-hand,
//! from a summary of SMS Power's SN76489 page that could not be read itself):
//! - Mk3: Sega Mark III Hardware Reference Manual rev 1, pp. 26-28.
//! - Plutiedev: <https://plutiedev.com/psg>.
//! - VGM: the VGM 1.71 format specification, fields 0x28 and 0x2A.
//! - Nemesis: SpritesMind forum, <http://gendev.spritesmind.net/forum/viewtopic.php?t=1132>.
//! - SMS Power: <https://www.smspower.org/Development/SN76489> [U].
//!
//! The chip only takes writes and makes a level; [`Psg::step`] advances it by one tick of
//! its counters, [`STEP_CLOCKS`] of its input clock.

/// Input clocks per counter tick: the chip divides its clock by 16 before the counters
/// (BigEvilCorp and Bumbershoot: frequency = clock / (32 x period); SMS Power [U]).
pub const STEP_CLOCKS: u64 = 16;

/// Tone channels; the noise channel is channel 3.
const TONES: usize = 3;

/// The noise channel's number in latch bytes.
const NOISE: usize = 3;

/// Amplitude per attenuation step, 2 dB apart, 15 off (Mk3 p. 28, Plutiedev): 10^(-n/10).
const VOLUME: [f32; 16] = [
    1.0,
    0.794_328_2,
    0.630_957_3,
    0.501_187_2,
    0.398_107_2,
    0.316_227_8,
    0.251_188_6,
    0.199_526_2,
    0.158_489_3,
    0.125_892_5,
    0.1,
    0.079_432_82,
    0.063_095_73,
    0.050_118_72,
    0.039_810_72,
    0.0,
];

/// Attenuation that silences a channel.
const OFF: u8 = 15;

/// The noise shift register as a noise register write leaves it: 16 bits wide on Sega's
/// part (VGM 0x2A), only the top bit set (SMS Power [U]).
const NOISE_RESET: u16 = 0x8000;

/// The shift register bits whose exclusive or feeds white noise: bits 0 and 3 (VGM 0x28,
/// mask 0x0009 for the Mega Drive).
const WHITE_TAPS: u16 = 0x0009;

/// One counter and the square wave it toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counter {
    /// Ticks left before the next toggle.
    left: u16,
    /// Whether the square wave is in its high half.
    high: bool,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            left: 1,
            high: true,
        }
    }
}

impl Counter {
    /// One tick: counts down and, on reaching zero, toggles the wave and reloads with
    /// `period` (Mk3 p. 26). A period of 0 acts as 1, the wave toggling every tick, on Sega's
    /// part (Nemesis; SMS Power's "constant output" is disputed there). Returns whether the
    /// wave toggled.
    fn tick(&mut self, period: u16) -> bool {
        self.left -= 1;
        if self.left > 0 {
            return false;
        }
        self.left = period.max(1);
        self.high = !self.high;
        true
    }
}

/// The PSG's registers and counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Psg {
    /// Each tone channel's 10-bit period, in ticks per half wave.
    periods: [u16; TONES],
    /// The noise control: bit 2 white (else periodic), bits 0-1 the rate.
    noise: u8,
    /// Attenuation per channel, tones then noise.
    attenuation: [u8; 4],
    /// The register the last latch byte chose: channel and whether it is the attenuation.
    latched: (usize, bool),
    tones: [Counter; TONES],
    noise_counter: Counter,
    shift: u16,
}

impl Default for Psg {
    /// The registers at power-on are not documented (the Mark III BIOS silences every
    /// channel itself, Mk3 p. 28): every channel off, so a game that never writes the PSG
    /// stays silent.
    fn default() -> Self {
        Self {
            periods: [0; TONES],
            noise: 0,
            attenuation: [OFF; 4],
            latched: (0, false),
            tones: [Counter::default(); TONES],
            noise_counter: Counter::default(),
            shift: NOISE_RESET,
        }
    }
}

impl Psg {
    /// Takes a byte written to the PSG's port.
    ///
    /// A latch byte, bit 7 set, is `1cctdddd`: it chooses channel `cc`'s attenuation (`t`
    /// set) or period or noise control, and writes `dddd` into its low bits. A data byte,
    /// bit 7 clear, writes its low six bits into the high six of a latched period (Mk3
    /// p. 26, Plutiedev, BigEvilCorp); into a latched attenuation or noise control, its low
    /// bits as a latch byte would (SMS Power: the latch is never cleared by a data byte [U]).
    /// A new period takes effect when the counter next reloads: no source says a write
    /// reloads it [U].
    pub fn write(&mut self, value: u8) {
        let data = u16::from(value & 0x0F);
        if value & 0x80 != 0 {
            self.latched = (usize::from(value >> 5 & 3), value & 0x10 != 0);
        }
        match self.latched {
            (channel, true) => self.attenuation[channel] = value & 0x0F,
            (NOISE, false) => self.write_noise(value & 0x07),
            (channel, false) if value & 0x80 != 0 => {
                self.periods[channel] = self.periods[channel] & 0x3F0 | data;
            }
            (channel, false) => {
                self.periods[channel] = u16::from(value & 0x3F) << 4 | self.periods[channel] & 0x0F;
            }
        }
    }

    /// Sets the noise control, which starts the shift register afresh.
    fn write_noise(&mut self, control: u8) {
        self.noise = control;
        self.shift = NOISE_RESET;
    }

    /// Advances every counter by one tick.
    pub fn step(&mut self) {
        for (counter, &period) in self.tones.iter_mut().zip(&self.periods) {
            counter.tick(period);
        }
        // Rates 0-2 reload the noise counter with 0x10, 0x20 or 0x40; rate 3 with tone 2's
        // period (Mk3 p. 27 "Tone Generator #3", SMS Power [U]).
        let period = match self.noise & 3 {
            3 => self.periods[2],
            rate => 0x10 << rate,
        };
        // The shift register moves on the wave's rising edge only, so at half the toggle
        // rate (SMS Power [U]).
        if self.noise_counter.tick(period) && self.noise_counter.high {
            let feedback = if self.noise & 4 != 0 {
                (self.shift & WHITE_TAPS).count_ones() & 1
            } else {
                u32::from(self.shift & 1)
            };
            self.shift = self.shift >> 1 | (feedback as u16) << 15;
        }
    }

    /// Channel `channel`'s level now, tones 0-2 then noise 3: plus or minus its amplitude,
    /// 0 when off. The noise channel plays bit 0 of the shift register (SMS Power [U]).
    pub fn channel(&self, channel: usize) -> f32 {
        let high = match self.tones.get(channel) {
            Some(counter) => counter.high,
            None => self.shift & 1 != 0,
        };
        let amplitude = VOLUME[usize::from(self.attenuation[channel])];
        if high { amplitude } else { -amplitude }
    }

    /// The four channels' levels added: from -4 to 4.
    pub fn output(&self) -> f32 {
        (0..=NOISE).map(|channel| self.channel(channel)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PSG with every channel at full volume and tone `channel` at `period`.
    fn tone(channel: u8, period: u16) -> Psg {
        let mut psg = Psg::default();
        psg.write(0x80 | channel << 5 | (period & 0x0F) as u8);
        psg.write((period >> 4) as u8);
        psg.write(0x90 | channel << 5);
        psg
    }

    /// The ticks between the first `count` toggles of tone `channel`.
    fn toggle_gaps(psg: &mut Psg, channel: usize, count: usize) -> Vec<u32> {
        let mut gaps = Vec::new();
        let mut ticks = 0;
        let mut high = psg.tones[channel].high;
        while gaps.len() < count {
            psg.step();
            ticks += 1;
            if psg.tones[channel].high != high {
                high = psg.tones[channel].high;
                gaps.push(ticks);
                ticks = 0;
            }
        }
        gaps
    }

    #[test]
    fn test_power_on_is_silent() {
        let mut psg = Psg::default();
        for _ in 0..1000 {
            psg.step();
            assert_eq!(psg.output(), 0.0);
        }
    }

    #[test]
    fn test_latch_and_data_bytes_make_a_ten_bit_period() {
        let psg = tone(1, 0x2A5);
        assert_eq!(psg.periods, [0, 0x2A5, 0]);
        let psg = tone(2, 0x3FF);
        assert_eq!(psg.periods[2], 0x3FF);
    }

    #[test]
    fn test_latch_byte_alone_changes_only_the_low_four_bits() {
        let mut psg = tone(0, 0x123);
        psg.write(0x8F);
        assert_eq!(psg.periods[0], 0x12F);
    }

    #[test]
    fn test_data_byte_goes_to_the_register_latched_last() {
        let mut psg = tone(0, 0x100);
        psg.write(0xA5);
        psg.write(0x3F);
        assert_eq!(psg.periods, [0x100, 0x3F5, 0]);
        psg.write(0xB0);
        psg.write(0x07);
        assert_eq!(psg.attenuation[1], 7);
        assert_eq!(psg.periods[1], 0x3F5);
    }

    #[test]
    fn test_tone_toggles_every_period_ticks() {
        let mut psg = tone(0, 5);
        let gaps = toggle_gaps(&mut psg, 0, 4);
        assert_eq!(gaps[1..], [5, 5, 5]);
    }

    #[test]
    fn test_period_zero_acts_as_one() {
        for period in [0, 1] {
            let mut psg = tone(0, period);
            assert_eq!(toggle_gaps(&mut psg, 0, 4), [1, 1, 1, 1], "period {period}");
        }
    }

    #[test]
    fn test_new_period_waits_for_the_next_reload() {
        let mut psg = tone(0, 100);
        psg.step();
        psg.step();
        let left = psg.tones[0].left;
        psg.write(0x82);
        psg.write(0x00);
        psg.step();
        assert_eq!(psg.tones[0].left, left - 1);
    }

    #[test]
    fn test_tone_level_swings_around_zero_by_the_volume() {
        let mut psg = tone(0, 3);
        let high = psg.output();
        assert_eq!(high, 1.0);
        toggle_gaps(&mut psg, 0, 1);
        assert_eq!(psg.output(), -1.0);
    }

    #[test]
    fn test_volume_steps_are_two_db_and_fifteen_is_off() {
        for (step, &amplitude) in VOLUME.iter().enumerate().take(15) {
            let expected = 10f32.powf(-(step as f32) / 10.0);
            assert!((amplitude - expected).abs() < 1e-6, "step {step}");
        }
        assert_eq!(VOLUME[15], 0.0);
        let mut psg = tone(2, 50);
        psg.write(0xD6);
        assert!((psg.channel(2) - VOLUME[6]).abs() < 1e-7);
        psg.write(0xDF);
        assert_eq!(psg.channel(2), 0.0);
    }

    #[test]
    fn test_noise_write_resets_the_shift_register() {
        let mut psg = Psg::default();
        psg.write(0xE4);
        for _ in 0..1000 {
            psg.step();
        }
        assert_ne!(psg.shift, NOISE_RESET);
        psg.write(0xE5);
        assert_eq!(psg.shift, NOISE_RESET);
        assert_eq!(psg.noise, 5);
    }

    /// The shift register's values over `shifts` shifts at noise control `control`.
    fn shifts(control: u8, shifts: usize) -> Vec<u16> {
        let mut psg = Psg::default();
        psg.write(0xE0 | control);
        let mut values = vec![psg.shift];
        while values.len() <= shifts {
            psg.step();
            if psg.shift != *values.last().expect("not empty") {
                values.push(psg.shift);
            }
        }
        values
    }

    #[test]
    fn test_white_noise_feeds_bit_0_xor_bit_3_into_bit_15() {
        let mut psg = Psg::default();
        psg.write(0xE4);
        psg.shift = 0x0009;
        for _ in 0..2 * 0x10 {
            psg.step();
        }
        assert_eq!(psg.shift, 0x0004, "1 xor 1 = 0");
        psg.shift = 0x0001;
        for _ in 0..2 * 0x10 {
            psg.step();
        }
        assert_eq!(psg.shift, 0x8000);
    }

    #[test]
    fn test_white_noise_is_high_about_half_the_time() {
        let mut psg = Psg::default();
        psg.write(0xE4);
        psg.write(0xF0);
        let ticks = 4096 * 2 * 0x10;
        let high = (0..ticks)
            .filter(|_| {
                psg.step();
                psg.channel(3) > 0.0
            })
            .count();
        assert!(
            (ticks * 2 / 5..ticks * 3 / 5).contains(&high),
            "{high} of {ticks}"
        );
    }

    #[test]
    fn test_periodic_noise_rotates_its_single_bit() {
        let values = shifts(0, 16);
        let expected: Vec<u16> = (0..=16)
            .map(|shift| 0x8000u16.rotate_right(shift))
            .collect();
        assert_eq!(values, expected);
    }

    #[test]
    fn test_periodic_noise_is_high_one_shift_in_sixteen() {
        let mut psg = Psg::default();
        psg.write(0xE0);
        psg.write(0xF0);
        let mut high = 0;
        let ticks = 16 * 2 * 0x10;
        for _ in 0..ticks {
            psg.step();
            if psg.channel(3) > 0.0 {
                high += 1;
            }
        }
        assert_eq!(high, ticks / 16);
    }

    #[test]
    fn test_noise_shifts_every_two_reloads_at_each_rate() {
        for (rate, reload) in [(0u8, 0x10), (1, 0x20), (2, 0x40)] {
            let mut psg = Psg::default();
            psg.write(0xE4 | rate);
            let mut ticks = 0;
            let mut gaps = Vec::new();
            let mut shift = psg.shift;
            while gaps.len() < 3 {
                psg.step();
                ticks += 1;
                if psg.shift != shift {
                    shift = psg.shift;
                    gaps.push(ticks);
                    ticks = 0;
                }
            }
            assert_eq!(gaps[1..], [2 * reload, 2 * reload], "rate {rate}");
        }
    }

    #[test]
    fn test_noise_rate_3_follows_tone_2() {
        let mut psg = tone(2, 7);
        psg.write(0xE7);
        let mut ticks = 0;
        let mut gaps = Vec::new();
        let mut shift = psg.shift;
        while gaps.len() < 3 {
            psg.step();
            ticks += 1;
            if psg.shift != shift {
                shift = psg.shift;
                gaps.push(ticks);
                ticks = 0;
            }
        }
        assert_eq!(gaps[1..], [14, 14]);
    }
}
