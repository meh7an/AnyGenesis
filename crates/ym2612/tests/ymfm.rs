//! Register scripts played into this crate and into ymfm, compared after every output
//! period with zero tolerance: each channel's 9-bit level on each side, and the status
//! register.
//!
//! ymfm mixes its six channels into one value per side; the shim reads each channel's level
//! before that mix instead, so a difference shows in the channel it is in. One test checks
//! that the levels put through this crate's DAC model and ymfm's 64/65 trim are ymfm's own
//! mix.

use anygenesis_oracle::ymfm;
use anygenesis_ym2612::{
    CHANNELS, Control, DAC_DATA, DAC_ENABLE, KEY_ON, LFO, Output, PAN, SAMPLE_CLOCKS, TEST,
    TIMER_A_HIGH, TIMER_A_LOW, TIMER_B, TIMER_CONTROL, Ym2612,
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
    /// The last writes as (period, port, value), for a failure's message.
    recent: Vec<(u64, u32, u8)>,
}

impl Pair {
    fn new() -> Self {
        Self {
            ymfm: ymfm::Ym2612::new(),
            control: Control::default(),
            chip: Ym2612::default(),
            periods: 0,
            recent: Vec::new(),
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
        if self.recent.len() == 32 {
            self.recent.remove(0);
        }
        self.recent.push((self.periods, port, value));
        self.ymfm.write(port, value);
        let written = self.control.write(port, value, self.clock());
        if let Some((part, register)) = written.data {
            self.chip.write(part, register, value);
        }
    }

    fn period(&mut self, at: &str) {
        let expected = self.ymfm.generate_channels();
        let ours = self.chip.sample();
        self.periods += 1;
        let period = self.periods;
        let levels = ours.channels.map(|levels| levels.map(i32::from));
        if levels != expected {
            let operators: Vec<_> = (0..24).map(|index| self.ymfm.operator(index)).collect();
            panic!(
                "{at}: levels of period {period}\n ours {levels:?}\nymfm {expected:?}\n\
                 ymfm operators {operators:?}\nlast writes (period, port, value) {:02X?}",
                self.recent
            );
        }
        let status = self.control.status(self.clock());
        assert_eq!(
            status,
            self.ymfm.status(),
            "{at}: status after period {period}"
        );
    }
}

/// `output` mixed as ymfm mixes a YM2612's channels: this crate's DAC model, then the
/// average of the six channels with a 64/65 trim.
fn ymfm_mix(output: &Output) -> [i32; 2] {
    output
        .sides()
        .map(|sum| sum * 128 * 64 / (CHANNELS as i32 * 65))
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

/// Each operator's register offset within its channel's, operators 1-4.
const OPERATOR_OFFSETS: [u8; 4] = [0, 8, 4, 12];

/// One channel's voice: per operator (1-4) the registers $30, $40, $50, $60, $70, $80 and
/// $90, then $B0 and $B4.
#[derive(Debug, Clone, Copy)]
struct Voice {
    operators: [[u8; 7]; 4],
    algorithm: u8,
    pan: u8,
}

impl Voice {
    /// A voice whose every operator sounds: a quick attack, a slow decay and a medium
    /// release, each operator a little different.
    fn plain(algorithm: u8) -> Self {
        Self {
            operators: std::array::from_fn(|operator| {
                let operator = operator as u8;
                [
                    0x01 + operator * 0x11,
                    0x08 + operator * 4,
                    0x1F - operator,
                    0x04 + operator,
                    0x02,
                    0x27 + operator * 0x10,
                    0,
                ]
            }),
            algorithm,
            pan: 0xC0,
        }
    }
}

/// Channel `channel`'s (0-5) part and register offset.
fn place(channel: usize) -> (u32, u8) {
    (1 + channel as u32 / 3, (channel % 3) as u8)
}

/// Writes `voice` to channel `channel`.
fn voice(channel: usize, voice: &Voice) -> Vec<Step> {
    let (part, offset) = place(channel);
    let mut script = Vec::new();
    for (operator, values) in voice.operators.iter().enumerate() {
        for (group, &value) in values.iter().enumerate() {
            let register = 0x30 + 0x10 * group as u8 + OPERATOR_OFFSETS[operator] + offset;
            script.extend(set(part, register, value));
        }
    }
    script.extend(set(part, 0xB0 + offset, voice.algorithm));
    script.extend(set(part, PAN + offset, voice.pan));
    script
}

/// Sets channel `channel`'s block and frequency number, the high byte first.
fn frequency(channel: usize, block: u8, number: u16) -> Vec<Step> {
    let (part, offset) = place(channel);
    let mut script = set(part, 0xA4 + offset, block << 3 | (number >> 8) as u8).to_vec();
    script.extend(set(part, 0xA0 + offset, number as u8));
    script
}

/// Keys channel `channel`'s operators in `operators` (bits 0-3, operators 1-4) on and the
/// rest off.
fn key(channel: usize, operators: u8) -> [Step; 2] {
    let code = (channel % 3) as u8 | ((channel / 3) as u8) << 2;
    set(1, KEY_ON, operators << 4 | code)
}

/// A note on channel `channel`: `sound`, a frequency, `on` periods keyed on and `off`
/// periods keyed off.
fn note(channel: usize, sound: &Voice, block: u8, number: u16, on: u32, off: u32) -> Vec<Step> {
    let mut script = voice(channel, sound);
    script.extend(frequency(channel, block, number));
    script.extend(key(channel, 0x0F));
    script.push(Step::Periods(on));
    script.extend(key(channel, 0));
    script.push(Step::Periods(off));
    script
}

#[test]
fn test_probe_levels_mix_to_ymfm_output() {
    let mut script = note(0, &Voice::plain(4), 4, 1083, 200, 100);
    script.extend(note(4, &Voice::plain(7), 3, 700, 100, 50));
    script.extend(set(1, DAC_ENABLE, 0x80));
    script.extend(set(1, DAC_DATA, 0x31));
    script.extend(note(5, &Voice::plain(2), 5, 900, 100, 50));
    let (mut mixed, mut probed) = (ymfm::Ym2612::new(), ymfm::Ym2612::new());
    for (index, &step) in script.iter().enumerate() {
        match step {
            Step::Write(port, value) => {
                mixed.write(port, value);
                probed.write(port, value);
            }
            Step::Periods(count) => {
                for _ in 0..count {
                    let mut expected = [0; 2];
                    mixed.generate(&mut expected);
                    let channels = probed.generate_channels().map(|levels| {
                        levels.map(|level| i16::try_from(level).expect("a 9-bit level"))
                    });
                    assert_eq!(ymfm_mix(&Output { channels }), expected, "step {index}");
                }
            }
        }
    }
}

#[test]
fn test_algorithms_and_feedback_match() {
    for algorithm in 0..8 {
        for feedback in 0..8 {
            let sound = Voice::plain(feedback << 3 | algorithm);
            let channel = usize::from(algorithm + feedback) % CHANNELS;
            let script = note(channel, &sound, 4, 1083, 120, 60);
            Pair::new().play(
                &format!("algorithm {algorithm} feedback {feedback}"),
                &script,
            );
        }
    }
}

#[test]
fn test_frequencies_detune_and_multiple_match() {
    let mut script = voice(1, &Voice::plain(7));
    script.extend(key(1, 0x0F));
    for block in 0..8 {
        for number in [0, 1, 0x155, 0x37F, 0x380, 0x400, 0x480, 0x5A5, 0x7FF] {
            script.extend(frequency(1, block, number));
            for detune_multiple in (0..0x80).step_by(9) {
                for offset in OPERATOR_OFFSETS {
                    script.extend(set(1, 0x31 + offset, detune_multiple + offset));
                }
                script.push(Step::Periods(3));
            }
        }
    }
    Pair::new().play("frequencies", &script);
}

#[test]
fn test_envelopes_match() {
    // Attack, decay 1, decay 2, sustain level and release.
    let rates = [
        (0x1F, 0x00, 0x00, 0x0, 0x0),
        (0x1E, 0x1F, 0x10, 0x7, 0xF),
        (0x14, 0x0C, 0x08, 0x3, 0x8),
        (0x0A, 0x06, 0x02, 0xF, 0x4),
        (0x04, 0x01, 0x01, 0x8, 0x1),
        (0x12, 0x00, 0x05, 0x0, 0x6),
    ];
    for (index, &(attack, decay, sustain_rate, level, release)) in rates.iter().enumerate() {
        for scale in 0..4u8 {
            let mut sound = Voice::plain(7);
            for operator in &mut sound.operators {
                operator[2] = scale << 6 | attack;
                operator[3] = decay;
                operator[4] = sustain_rate;
                operator[5] = level << 4 | release;
            }
            let mut script = note(3, &sound, 7, 0x6A0, 1500, 600);
            // A key on in the release, then a key off and on of some operators.
            script.extend(key(3, 0x0F));
            script.push(Step::Periods(300));
            script.extend(key(3, 0x05));
            script.push(Step::Periods(200));
            script.extend(key(3, 0x0F));
            script.push(Step::Periods(400));
            Pair::new().play(&format!("envelope {index} scale {scale}"), &script);
        }
    }
}

#[test]
fn test_total_level_and_slow_rates_match() {
    let mut sound = Voice::plain(7);
    for (operator, values) in sound.operators.iter_mut().enumerate() {
        values[1] = [0x00, 0x20, 0x45, 0x7F][operator];
        values[2] = 0x03;
        values[3] = 0x02;
        values[5] = 0x51;
    }
    Pair::new().play("slow", &note(0, &sound, 2, 0x300, 20_000, 8_000));
}

#[test]
fn test_lfo_matches() {
    for rate in 0..8u8 {
        let mut sound = Voice::plain(5);
        for operator in &mut sound.operators {
            operator[3] |= 0x80;
        }
        let mut script = set(1, LFO, 0x08 | rate).to_vec();
        for depth in 0..8u8 {
            sound.pan = 0xC0 | (depth & 0x03) << 4 | depth;
            let number = 0x2C0 + 0x55 * u16::from(depth);
            script.extend(note(0, &sound, 1 + depth % 7, number, 900, 0));
        }
        script.extend(set(1, LFO, 0x00));
        script.push(Step::Periods(100));
        Pair::new().play(&format!("lfo rate {rate}"), &script);
    }
}

#[test]
fn test_lfo_off_with_amplitude_modulation_on_matches() {
    let mut sound = Voice::plain(7);
    sound.pan = 0xF0;
    for operator in &mut sound.operators {
        operator[3] |= 0x80;
    }
    Pair::new().play("lfo off", &note(2, &sound, 4, 0x500, 300, 100));
}

#[test]
fn test_channel_3_special_mode_matches() {
    let mut script = voice(2, &Voice::plain(7));
    script.extend(frequency(2, 4, 0x400));
    for (index, (high, low)) in [(0xAD, 0xA9), (0xAE, 0xAA), (0xAC, 0xA8)]
        .into_iter()
        .enumerate()
    {
        script.extend(set(1, high, (3 + index as u8) << 3 | 0x02));
        script.extend(set(1, low, 0x30 * index as u8));
    }
    script.extend(key(2, 0x0F));
    script.push(Step::Periods(200));
    script.extend(set(1, TIMER_CONTROL, 0x40));
    script.push(Step::Periods(300));
    // A high byte waits for its low byte.
    script.extend(set(1, 0xAC, 0x3F));
    script.push(Step::Periods(50));
    script.extend(set(1, 0xA8, 0x11));
    script.push(Step::Periods(50));
    script.extend(set(1, TIMER_CONTROL, 0x00));
    script.push(Step::Periods(100));
    Pair::new().play("channel 3 special", &script);
}

#[test]
fn test_csm_matches() {
    let mut sound = Voice::plain(7);
    for operator in &mut sound.operators {
        operator[5] = 0x0F;
    }
    let mut script = voice(2, &sound);
    script.extend(frequency(2, 4, 0x400));
    script.extend(set(1, TIMER_A_HIGH, 0xF8));
    script.extend(set(1, TIMER_CONTROL, 0x85));
    // ymfm takes key changes only when a register is written; a write every period puts
    // its CSM key off at the next period, where this crate puts it.
    for _ in 0..800 {
        script.extend(set(1, 0x40, 0x7F));
        script.push(Step::Periods(1));
    }
    Pair::new().play("csm", &script);
}

#[test]
fn test_ssg_eg_shapes_match() {
    for shape in 0..8u8 {
        let mut sound = Voice::plain(7);
        for operator in &mut sound.operators {
            operator[2] = 0x1F;
            operator[3] = 0x0C;
            operator[4] = 0x08;
            operator[6] = 0x08 | shape;
        }
        let script = note(1, &sound, 4, 0x400, 2000, 300);
        Pair::new().play(&format!("ssg-eg shape {shape}"), &script);
    }
}

#[test]
fn test_dac_in_place_of_channel_6_fm_matches() {
    let mut script = note(5, &Voice::plain(1 << 3 | 4), 4, 0x380, 0, 0);
    script.extend(key(5, 0x0F));
    script.extend(set(1, DAC_DATA, 0x80));
    script.push(Step::Periods(50));
    script.extend(set(1, DAC_ENABLE, 0x80));
    script.extend(set(1, DAC_DATA, 0xD0));
    script.push(Step::Periods(50));
    script.extend(set(1, DAC_ENABLE, 0x00));
    script.push(Step::Periods(100));
    Pair::new().play("dac over fm", &script);
}

#[test]
fn test_random_fm_scripts_match() {
    for seed in 1..=30_u32 {
        let mut random = XorShift(seed.wrapping_mul(0x2545_F491));
        let mut script = Vec::new();
        for channel in 0..CHANNELS {
            let sound = Voice::plain(random.below(8) as u8);
            script.extend(note(channel, &sound, 4, 0x400, 0, 0));
        }
        for _ in 0..400 {
            match random.below(8) {
                0 | 1 => script.extend(key(random.below(6) as usize, random.byte() & 0x0F)),
                2 => {
                    let channel = random.below(6) as usize;
                    let number = random.next() as u16 & 0x7FF;
                    script.extend(frequency(channel, random.below(8) as u8, number));
                }
                3 => script.extend(set(1, LFO, random.byte() & 0x0F)),
                _ => {
                    // Any operator or channel register but SSG-EG's.
                    let register = match 0x30 + random.below(0x90) as u8 {
                        ssg @ 0x90..=0x9F => ssg + 0x20,
                        register => register,
                    };
                    script.extend(set(1 + random.below(2), register, random.byte()));
                }
            }
            script.push(Step::Periods(random.below(200)));
        }
        Pair::new().play(&format!("random fm {seed}"), &script);
    }
}

/// The periods of `script` whose levels differ between the two, for behaviour ymfm is not
/// known to model like the chip.
fn mismatched_periods(script: &[Step]) -> usize {
    let mut pair = Pair::new();
    let mut mismatched = 0;
    for &step in script {
        match step {
            Step::Write(port, value) => pair.write(port, value),
            Step::Periods(count) => {
                for _ in 0..count {
                    let expected = pair.ymfm.generate_channels();
                    let ours = pair
                        .chip
                        .sample()
                        .channels
                        .map(|levels| levels.map(i32::from));
                    mismatched += usize::from(ours != expected);
                }
            }
        }
    }
    mismatched
}

/// SSG-EG where the documents call the result undefined or untested and ymfm has open
/// issues (#9, #49, #50): an attack rate below $1F, a key on in the release, the shape
/// changed mid-note. Reported, not failing (Mehran's tolerance, 2026-10-29).
#[test]
fn test_ssg_eg_edge_cases_are_reported() {
    for shape in 0..8u8 {
        let mut sound = Voice::plain(7);
        for (operator, values) in sound.operators.iter_mut().enumerate() {
            values[2] = 0x18 + operator as u8;
            values[3] = 0x0C;
            values[6] = 0x08 | shape;
        }
        let mut script = note(1, &sound, 4, 0x400, 1500, 200);
        script.extend(key(1, 0x0F));
        script.push(Step::Periods(500));
        script.extend(set(1, 0x91, 0x08 | (shape ^ 0x05)));
        script.push(Step::Periods(800));
        let mismatched = mismatched_periods(&script);
        println!("ssg-eg shape {shape} edge cases: {mismatched} of 3000 periods differ");
    }
}

/// Puts both chips' DACs where the other's is: ymfm's starts at its centre and its reset
/// leaves the DAC alone, where this crate's starts at sample 0 and its reset turns it off.
fn align_dacs(ymfm: &mut ymfm::Ym2612, chip: &mut Ym2612) {
    for (register, value) in [(DAC_DATA, 0x80), (DAC_ENABLE, 0x00)] {
        ymfm.set(0, register, value);
        chip.write(1, register, value);
    }
}

/// A sound log the player wrote with `--sound-log` (a game's own writes, so it stays
/// outside the repository), played into both: every period's levels must match.
#[test]
#[ignore = "needs ANYGENESIS_SOUND_LOG naming a player sound log (an absolute path)"]
fn test_sound_log_matches() {
    let path = std::env::var("ANYGENESIS_SOUND_LOG").expect("ANYGENESIS_SOUND_LOG is set");
    let log = std::fs::read_to_string(&path).expect("the sound log reads");
    let (mut ymfm, mut chip) = (ymfm::Ym2612::new(), Ym2612::default());
    align_dacs(&mut ymfm, &mut chip);
    let (mut periods, mut writes) = (0u64, 0u64);
    let mut recent: Vec<&str> = Vec::new();
    for line in log.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [clock, _, chip_name, register, value] = fields[..] else {
            panic!("a sound log line: {line}");
        };
        let clock: u64 = clock.parse().expect("a master clock");
        while (periods + 1) * SAMPLE_CLOCKS <= clock {
            let expected = ymfm.generate_channels();
            let ours = chip.sample().channels.map(|levels| levels.map(i32::from));
            periods += 1;
            if ours != expected {
                let operators: Vec<_> = (0..24).map(|index| ymfm.operator(index)).collect();
                let channel = (0..CHANNELS)
                    .find(|&channel| ours[channel] != expected[channel])
                    .expect("a channel differs");
                let ours_operators: Vec<_> = (0..4)
                    .map(|operator| chip.operator_state(channel, operator))
                    .collect();
                panic!(
                    "period {periods}, after {writes} writes\n ours {ours:?}\nymfm {expected:?}\n\
                     channel {channel}'s operators here {ours_operators:?}\n\
                     ymfm operators {operators:?}\nlast writes {recent:?}"
                );
            }
        }
        let byte = |text| u8::from_str_radix(text, 16).expect("a hex byte");
        match chip_name {
            "ym1" | "ym2" => {
                if register != "2A" {
                    if recent.len() == 40 {
                        recent.remove(0);
                    }
                    recent.push(line);
                }
                let part = if chip_name == "ym1" { 1 } else { 2 };
                ymfm.set(u32::from(part - 1), byte(register), byte(value));
                chip.write(part, byte(register), byte(value));
                writes += 1;
            }
            "ymreset" => {
                ymfm.reset();
                chip.reset();
                align_dacs(&mut ymfm, &mut chip);
            }
            _ => {}
        }
    }
    println!("{writes} writes over {periods} periods matched");
}
