//! FM synthesis: six channels of four operators, wired by one of eight algorithms, with the
//! first operator fed back into itself, and the envelope clock and LFO they share.
//!
//! Sources (side-session note v0.2-m3.5-research-fm.md has the quotes; SpritesMind posts
//! are in thread t=386 unless named): the eight algorithms (YM2608 manual 2-4, its figure
//! read through Sauraen's modulation sources, p=27098); feedback as operator 1's last two
//! outputs added (Sauraen, p=27319) and shifted so FB 1-7 moves the phase by pi/16 to
//! 4 pi (manual table 2-3, Eke p=6096); the key code (manual 2-5, Nemesis p=6177); the
//! phase step `(fnum << block) >> 1`, detuned in 17 bits, then multiplied, multiple 0 a
//! half (p=6177); rates `2R + (key code >> (3 - KS))`, release `2RR + 1`, 0 standing
//! still, 63 at most (manual 2-6 and table 2-8, Nemesis p=5716); sustain level 15 at 93 dB
//! (manual table 2-7); total level in 0.75 dB steps (manual table 2-9); the envelope
//! clock every third sample (Nemesis p=6225, p=7967); channel 3's frequency pairs
//! (manual 2-1 register table, Plutiedev); a modulator's 14-bit level moving the next
//! phase by its half (Nemesis, t=518 p=7795); the channel level the carriers' top 9 bits,
//! added with clipping (Sauraen p=29516, p=30088); the amplitude modulation's depths of
//! 1.4, 5.9 and 11.8 dB as the 7-bit LFO triangle shifted 3, 1 and 0 places (manual 2-7).
//! Settled by ymfm [O]: the clipping range -257..256 after each carrier added from
//! operator 4 on (the documents leave +255 or +256 open), and channel 6's FM not worked
//! out while the DAC plays in its place.

use crate::{
    lfo::{Lfo, Modulation},
    operator::{Operator, Params},
    registers::{
        ALGORITHM, DECAY, DETUNE_MULTIPLE, KEY_SCALE_ATTACK, LFO, PAN, Registers, SSG_EG,
        SUSTAIN_RATE, SUSTAIN_RELEASE, TIMER_CONTROL, TOTAL_LEVEL,
    },
    tables::{detune, phase_modulation},
};

/// FM channels: three per part.
pub const CHANNELS: usize = 6;

/// Operators per channel.
pub(crate) const OPERATORS: usize = 4;

/// Samples per envelope cycle.
const ENVELOPE_DIVIDER: u8 = 3;

/// The channel whose operators can each have a frequency of their own.
const SPECIAL_CHANNEL: usize = 2;

/// The 9-bit channel level's range, which each carrier's sum is held to.
const LEVEL_MIN: i32 = -257;
const LEVEL_MAX: i32 = 256;

/// Bits of a channel's 14-bit operator levels below its 9-bit output.
const OUTPUT_SHIFT: u32 = 5;

/// What each algorithm feeds operators 2, 3 and 4 with and which operators it adds to the
/// output (YM2608 manual 2-4).
#[derive(Debug, Clone, Copy)]
struct Algorithm {
    /// What modulates operators 2, 3 and 4.
    two: Input,
    three: Input,
    four: Input,
    /// Operators 1, 2 and 3 are carriers too; operator 4 always is.
    carriers: [bool; 3],
}

/// What an operator's phase is modulated by.
#[derive(Debug, Clone, Copy)]
enum Input {
    Zero,
    One,
    Two,
    Three,
    OneTwo,
    OneThree,
    TwoThree,
}

const ALGORITHMS: [Algorithm; 8] = {
    use Input::*;
    const fn algorithm(two: Input, three: Input, four: Input, carriers: [bool; 3]) -> Algorithm {
        Algorithm {
            two,
            three,
            four,
            carriers,
        }
    }
    [
        algorithm(One, Two, Three, [false, false, false]),
        algorithm(Zero, OneTwo, Three, [false, false, false]),
        algorithm(Zero, Two, OneThree, [false, false, false]),
        algorithm(One, Zero, TwoThree, [false, false, false]),
        algorithm(One, Zero, Three, [false, true, false]),
        algorithm(One, One, One, [false, true, true]),
        algorithm(One, Zero, Zero, [false, true, true]),
        algorithm(Zero, Zero, Zero, [true, true, true]),
    ]
};

/// One channel's state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Channel {
    operators: [Operator; OPERATORS],
    /// Operator 1's last two outputs, oldest first, for the feedback.
    feedback: [i32; 2],
    /// Operator 1's output this sample, which joins the feedback at the next.
    output_one: i32,
}

/// The FM half of the chip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Fm {
    channels: [Channel; CHANNELS],
    /// Each operator's parameters as the registers last gave them.
    params: [[Params; OPERATORS]; CHANNELS],
    /// No register has been written since `params` was worked out.
    fresh: bool,
    lfo: Lfo,
    /// Samples since the last envelope cycle.
    divider: u8,
    /// Envelope cycles since power-on.
    cycle: u32,
}

impl Fm {
    /// The channels and the LFO back to power-on; the envelope clock runs on [O].
    pub fn reset(&mut self) {
        *self = Self {
            divider: self.divider,
            cycle: self.cycle,
            ..Self::default()
        };
    }

    /// A register was written: the parameters are worked out again at the next sample.
    pub fn invalidate(&mut self) {
        self.fresh = false;
    }

    /// Operator `operator` (0-3) of channel `channel` (0-5).
    pub fn operator(&self, channel: usize, operator: usize) -> &Operator {
        &self.channels[channel].operators[operator]
    }

    /// Moves every channel on one sample, with each operator's key as `keys` has it, and
    /// gives each channel's 9-bit level on the left and right; channel 6's is not worked
    /// out while `dac` plays in its place.
    pub fn sample(
        &mut self,
        registers: &Registers,
        keys: &[[bool; OPERATORS]; CHANNELS],
        dac: bool,
    ) -> [[i16; 2]; CHANNELS] {
        self.divider += 1;
        let cycle = (self.divider == ENVELOPE_DIVIDER).then(|| {
            self.divider = 0;
            self.cycle = self.cycle.wrapping_add(1);
            self.cycle
        });
        let modulation = self.lfo.clock(registers.get(1, LFO));
        let mut levels = [[0; 2]; CHANNELS];
        for (index, (channel, params)) in self.channels.iter_mut().zip(&mut self.params).enumerate()
        {
            // The LFO moves the phase steps of a channel with a phase modulation depth.
            if !self.fresh || registers.channel(index, PAN) & 0x07 != 0 {
                *params = operator_params(registers, index, modulation);
            }
            channel.clock(&keys[index], cycle, params);
            if dac && index == CHANNELS - 1 {
                continue;
            }
            let level = channel.level(registers, index, modulation, params);
            let pan = registers.channel(index, PAN);
            levels[index] = [pan & 0x80 != 0, pan & 0x40 != 0].map(|on| if on { level } else { 0 });
        }
        self.fresh = true;
        levels
    }
}

impl Channel {
    fn clock(&mut self, keys: &[bool; OPERATORS], cycle: Option<u32>, params: &[Params; 4]) {
        for (operator, (&key, params)) in self.operators.iter_mut().zip(keys.iter().zip(params)) {
            operator.take_key(key, params);
        }
        self.feedback = [self.feedback[1], self.output_one];
        for (operator, params) in self.operators.iter_mut().zip(params) {
            operator.clock(cycle, params);
        }
    }

    /// The channel's 9-bit level this sample.
    fn level(
        &mut self,
        registers: &Registers,
        index: usize,
        modulation: Modulation,
        params: &[Params; 4],
    ) -> i16 {
        let algorithm_register = registers.channel(index, ALGORITHM);
        let algorithm = ALGORITHMS[usize::from(algorithm_register & 0x07)];
        let feedback = algorithm_register >> 3 & 0x07;
        let amplitude = amplitude_modulation(registers.channel(index, PAN), modulation);
        let [one, two, three, four] = &self.operators;
        if (0..OPERATORS)
            .all(|operator| self.operators[operator].silent(amplitude, &params[operator]))
        {
            self.output_one = 0;
            return 0;
        }

        let self_modulation = if feedback == 0 {
            0
        } else {
            (self.feedback[0] + self.feedback[1]) >> (10 - feedback)
        };
        let o1 = one.level(self_modulation, amplitude, &params[0]);
        self.output_one = o1;
        let input = |input: Input, o2: i32, o3: i32| {
            let sum = match input {
                Input::Zero => 0,
                Input::One => o1,
                Input::Two => o2,
                Input::Three => o3,
                Input::OneTwo => o1 + o2,
                Input::OneThree => o1 + o3,
                Input::TwoThree => o2 + o3,
            };
            sum >> 1
        };
        let o2 = two.level(input(algorithm.two, 0, 0), amplitude, &params[1]);
        let o3 = three.level(input(algorithm.three, o2, 0), amplitude, &params[2]);
        let o4 = four.level(input(algorithm.four, o2, o3), amplitude, &params[3]);

        let mut level = o4 >> OUTPUT_SHIFT;
        for (carrier, output) in algorithm.carriers.into_iter().zip([o1, o2, o3]) {
            if carrier {
                level = (level + (output >> OUTPUT_SHIFT)).clamp(LEVEL_MIN, LEVEL_MAX);
            }
        }
        level as i16
    }
}

/// What channel `channel`'s four operators' registers say this sample, the LFO's phase
/// modulation applied.
fn operator_params(registers: &Registers, channel: usize, modulation: Modulation) -> [Params; 4] {
    let pan = registers.channel(channel, PAN);
    let special = channel == SPECIAL_CHANNEL && registers.get(1, TIMER_CONTROL) & 0xC0 != 0;
    std::array::from_fn(|operator| {
        let frequency = match operator {
            // Operators 1, 2 and 3 take pairs $A9/$AD, $AA/$AE and $A8/$AC.
            0..3 if special => registers.special_frequency([1, 2, 0][operator]),
            _ => registers.frequency(channel),
        };
        let key_code = key_code(frequency);
        let read = |base| registers.operator(channel, operator, base);
        let detune_multiple = read(DETUNE_MULTIPLE);
        let key_scale_attack = read(KEY_SCALE_ATTACK);
        let decay = read(DECAY);
        let sustain_release = read(SUSTAIN_RELEASE);
        let ssg = read(SSG_EG);

        let scale = key_code >> (3 - (key_scale_attack >> 6));
        let rate = |raw: u8| if raw == 0 { 0 } else { (raw + scale).min(63) };
        let sustain_level = u16::from(sustain_release >> 4);
        // Sustain level 15 stands for 31: the lowest, 93 dB down.
        let sustain_level = sustain_level | (sustain_level + 1) & 0x10;
        Params {
            phase_step: phase_step(frequency, key_code, detune_multiple, pan, modulation),
            rates: [
                rate((key_scale_attack & 0x1F) * 2),
                rate((decay & 0x1F) * 2),
                rate((read(SUSTAIN_RATE) & 0x1F) * 2),
                rate((sustain_release & 0x0F) * 4 + 2),
            ],
            sustain: sustain_level << 5,
            total_level: u16::from(read(TOTAL_LEVEL) & 0x7F) << 3,
            amplitude_modulation: decay & 0x80 != 0,
            ssg: (ssg & 0x08 != 0).then_some(ssg & 0x07),
        }
    })
}

/// The 5-bit key code of a block and frequency number: the block, the number's top bit and
/// a bit from its next three (YM2608 manual 2-5).
fn key_code(frequency: u16) -> u8 {
    let bit = |n: u32| frequency >> n & 1 != 0;
    let (f11, f10, f9, f8) = (bit(10), bit(9), bit(8), bit(7));
    let low = (f11 && (f10 || f9 || f8)) || (!f11 && f10 && f9 && f8);
    (frequency >> 9) as u8 & 0x1E | u8::from(low)
}

/// The phase step of a block and frequency number: the number moved by the LFO, shifted
/// by the block, detuned, held to 17 bits and multiplied.
fn phase_step(
    frequency: u16,
    key_code: u8,
    detune_multiple: u8,
    pan: u8,
    modulation: Modulation,
) -> u32 {
    let mut number = u32::from(frequency & 0x7FF) << 1;
    let sensitivity = pan & 0x07;
    if sensitivity != 0 {
        let top = u32::from(frequency >> 4 & 0x7F);
        number = number.wrapping_add_signed(phase_modulation(top, sensitivity, modulation.phase));
        number &= 0xFFF;
    }
    let block = u32::from(frequency >> 11 & 0x07);
    let step = ((number << block) >> 2).wrapping_add_signed(detune(key_code, detune_multiple >> 4));
    // Multiple 0 is one half: the multiple works in halves.
    let multiple = match u32::from(detune_multiple & 0x0F) * 2 {
        0 => 1,
        doubled => doubled,
    };
    ((step & 0x1FFFF) * multiple) >> 1
}

/// The LFO's amplitude modulation at the channel's depth (bits 4-5 of its panning
/// register): none, then 1/8, 1/2 and all of the 7-bit triangle.
fn amplitude_modulation(pan: u8, modulation: Modulation) -> u16 {
    const SHIFTS: [u32; 4] = [7, 3, 1, 0];
    u16::from(modulation.amplitude) << 1 >> SHIFTS[usize::from(pan >> 4 & 0x03)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_code_takes_the_block_top_bit_and_the_manual_bit() {
        assert_eq!(key_code(0), 0);
        assert_eq!(key_code(0x0380), 1);
        assert_eq!(key_code(0x0400), 2);
        assert_eq!(key_code(0x0480), 3);
        assert_eq!(key_code(0x3FFF), 31);
        assert_eq!(key_code(4 << 11 | 1083), 4 << 2 | 2);
    }

    #[test]
    fn test_phase_step_doubles_per_block_and_halves_at_multiple_0() {
        let none = Modulation {
            amplitude: 0,
            phase: 0,
        };
        let step = |frequency, multiple| phase_step(frequency, 0, multiple, 0, none);
        assert_eq!(step(1 << 11 | 1000, 1), 1000);
        assert_eq!(step(2 << 11 | 1000, 1), 2000);
        assert_eq!(step(1 << 11 | 1000, 0), 500);
        assert_eq!(step(1 << 11 | 1000, 3), 3000);
    }
}
