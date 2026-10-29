//! Register scripts played into this crate and into ymfm, compared after every output
//! period with zero tolerance: the status register and the stereo output.
//!
//! ymfm mixes its six channels into one value per side, with an offset on each channel (its
//! model of the YM2612's DAC discontinuity) and a 64/65 trim; the comparison puts this
//! crate's per-channel levels through that same arithmetic, so a channel's level has to be
//! exact for the mix to match.

use anygenesis_oracle::ymfm;
use anygenesis_ym2612::{
    CHANNELS, Control, DAC_DATA, DAC_ENABLE, Output, PAN, SAMPLE_CLOCKS, TEST, TIMER_A_HIGH,
    TIMER_A_LOW, TIMER_B, TIMER_CONTROL, Ym2612,
};

/// One step of a script.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// A write to one of the four ports, between two output periods.
    Write(u32, u8),
    /// This many output periods.
    Periods(u32),
}

/// A register write to `part` (1 or 2): the address, then the data.
fn set(part: u32, register: u8, value: u8) -> [Step; 2] {
    let port = 2 * (part - 1);
    [Step::Write(port, register), Step::Write(port + 1, value)]
}

/// Both chips, with this crate's two halves fed the way the runtime and the mixer feed
/// them.
struct Pair {
    ymfm: ymfm::Ym2612,
    control: Control,
    chip: Ym2612,
    periods: u64,
}

impl Pair {
    fn new() -> Self {
        Self {
            ymfm: ymfm::Ym2612::new(),
            control: Control::default(),
            chip: Ym2612::default(),
            periods: 0,
        }
    }

    fn clock(&self) -> u64 {
        self.periods * SAMPLE_CLOCKS
    }

    /// Plays `script`, panicking at the first period where the two differ.
    fn play(&mut self, name: &str, script: &[Step]) {
        for (index, &step) in script.iter().enumerate() {
            match step {
                Step::Write(port, value) => self.write(port, value),
                Step::Periods(count) => {
                    for _ in 0..count {
                        self.period(&format!("{name}, step {index}"));
                    }
                }
            }
        }
    }

    fn write(&mut self, port: u32, value: u8) {
        self.ymfm.write(port, value);
        let written = self.control.write(port, value, self.clock());
        if let Some((part, register)) = written.data {
            self.chip.write(part, register, value);
        }
    }

    fn period(&mut self, at: &str) {
        let mut expected = [0; 2];
        self.ymfm.generate(&mut expected);
        let ours = ymfm_mix(&self.chip.sample());
        self.periods += 1;
        let period = self.periods;
        assert_eq!(ours, expected, "{at}: output of period {period}");
        let status = self.control.status(self.clock());
        assert_eq!(
            status,
            self.ymfm.status(),
            "{at}: status after period {period}"
        );
    }
}

/// `output` mixed as ymfm mixes a YM2612's channels.
fn ymfm_mix(output: &Output) -> [i32; 2] {
    let discontinuity = |level: i32| if level < 0 { level - 3 } else { level + 4 };
    [0, 1].map(|side| {
        let sum: i32 = output
            .channels
            .iter()
            .map(|levels| discontinuity(i32::from(levels[side])))
            .sum();
        sum * 128 * 64 / (CHANNELS as i32 * 65)
    })
}

/// A small deterministic generator for long scripts.
struct XorShift(u32);

impl XorShift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

#[test]
fn test_silence_matches() {
    Pair::new().play("silence", &[Step::Periods(64)]);
}

#[test]
fn test_dac_samples_match() {
    let mut script = Vec::new();
    script.extend(set(1, DAC_DATA, 0xFF));
    script.push(Step::Periods(2));
    script.extend(set(1, DAC_ENABLE, 0x80));
    for sample in (0..=255).step_by(5) {
        script.extend(set(1, DAC_DATA, sample));
        script.push(Step::Periods(1 + u32::from(sample % 3)));
    }
    script.extend(set(1, TEST, 0x08));
    script.push(Step::Periods(3));
    script.extend(set(1, DAC_DATA, 0x00));
    script.push(Step::Periods(2));
    script.extend(set(1, TEST, 0x00));
    script.push(Step::Periods(2));
    script.extend(set(1, DAC_ENABLE, 0x00));
    script.push(Step::Periods(4));
    Pair::new().play("dac", &script);
}

#[test]
fn test_dac_panning_matches() {
    let mut script = Vec::new();
    script.extend(set(1, DAC_ENABLE, 0x80));
    script.extend(set(1, DAC_DATA, 0x20));
    for pan in [0xC0, 0x80, 0x40, 0x00, 0x3F, 0xC0] {
        script.extend(set(2, PAN + 2, pan));
        script.push(Step::Periods(3));
        script.extend(set(1, PAN + 2, !pan));
        script.extend(set(2, PAN, 0x00));
        script.push(Step::Periods(2));
    }
    Pair::new().play("panning", &script);
}

#[test]
fn test_timers_match() {
    let mut script = Vec::new();
    script.extend(set(1, TIMER_A_HIGH, 0xFE));
    script.extend(set(1, TIMER_A_LOW, 0x03));
    script.extend(set(1, TIMER_B, 0xFD));
    script.extend(set(1, TIMER_CONTROL, 0x0F));
    script.push(Step::Periods(100));
    script.extend(set(1, TIMER_CONTROL, 0x3F));
    script.push(Step::Periods(7));
    script.extend(set(1, TIMER_A_HIGH, 0xF0));
    script.extend(set(1, TIMER_CONTROL, 0x1B));
    script.push(Step::Periods(90));
    script.extend(set(1, TIMER_CONTROL, 0x07));
    script.push(Step::Periods(70));
    script.extend(set(1, TIMER_CONTROL, 0x30));
    script.push(Step::Periods(20));
    Pair::new().play("timers", &script);
}

#[test]
fn test_busy_matches() {
    let mut script = Vec::new();
    for value in 0..8 {
        script.extend(set(1, 0x30 + value, value));
        script.push(Step::Periods(u32::from(value % 3)));
    }
    script.extend(set(2, 0x40, 0x7F));
    script.push(Step::Periods(4));
    Pair::new().play("busy", &script);
}

#[test]
fn test_random_timer_scripts_match() {
    let registers = [TIMER_A_HIGH, TIMER_A_LOW, TIMER_B, TIMER_CONTROL];
    for seed in 1..=40_u32 {
        let mut random = XorShift(seed.wrapping_mul(0x9E37_79B9));
        let mut script = Vec::new();
        script.extend(set(1, TIMER_A_HIGH, 0xF0 | random.byte()));
        script.extend(set(1, TIMER_B, 0xF0 | random.byte()));
        for _ in 0..200 {
            let register = registers[random.below(4) as usize];
            let value = match register {
                // Mostly short periods, so overflows happen within the script.
                TIMER_A_HIGH | TIMER_B => 0xF0 | random.byte(),
                // Mode bits stay 00 or 01: CSM keys channel 3 on, which is FM.
                TIMER_CONTROL => random.byte() & 0x7F,
                _ => random.byte(),
            };
            script.extend(set(1, register, value));
            script.push(Step::Periods(random.below(40)));
        }
        Pair::new().play(&format!("random timers {seed}"), &script);
    }
}

#[test]
fn test_random_dac_scripts_match() {
    for seed in 1..=20_u32 {
        let mut random = XorShift(seed.wrapping_mul(0x85EB_CA6B));
        // ymfm's DAC starts at its centre, the power-on register value 0 at its lowest.
        let mut script = set(1, DAC_DATA, 0x80).to_vec();
        for _ in 0..500 {
            let [address, data] = match random.below(16) {
                0 => set(1, DAC_ENABLE, random.byte()),
                1 => set(1, TEST, random.byte() & 0x08),
                2 => set(2, PAN + 2, random.byte()),
                3 => set(
                    1 + random.below(2),
                    PAN + random.below(3) as u8,
                    random.byte(),
                ),
                _ => set(1, DAC_DATA, random.byte()),
            };
            script.extend([address, data, Step::Periods(random.below(4))]);
        }
        Pair::new().play(&format!("random dac {seed}"), &script);
    }
}
