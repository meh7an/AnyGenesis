//! The chip's tables, each from its formula where a document gives one; the sources and
//! quotes are in the side-session note v0.2-m3.5-research-fm.md.

use std::sync::LazyLock;

/// A quarter of a sine wave as attenuation, 4.8 fixed point: `-log2(sin((2i + 1) x pi /
/// 1024)) x 256`, rounded; the half step keeps the sine above 0 at the first entry (Nemesis
/// on SpritesMind, t=386 start=150, p=6114).
static LOG_SINE: LazyLock<[u16; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let angle = (2 * index + 1) as f64 * std::f64::consts::PI / 1024.0;
        (-angle.sin().log2() * 256.0).round() as u16
    })
});

/// The fraction of 2^-x as a 10-bit mantissa below an implied 1: `2^(1 - (i + 1) / 256) x
/// 1024 - 1024`, rounded (Nemesis, t=386 start=150).
static POWER: LazyLock<[u16; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let power = (1.0 - (index + 1) as f64 / 256.0).exp2() * 1024.0;
        power.round() as u16 - 1024
    })
});

/// The sine wave at a 10-bit `position` as attenuation (4.8) and whether it is negative:
/// the quarter table read forwards, then backwards, then both again below 0 (p=6114 has
/// the quarter table mirrored by the top two bits; which bit does what is ymfm's [O]).
pub(crate) fn sine(position: u32) -> (u16, bool) {
    let mut quarter = position & 0xFF;
    if position & 0x100 != 0 {
        quarter ^= 0xFF;
    }
    (LOG_SINE[quarter as usize], position & 0x200 != 0)
}

/// A 13-bit level from attenuation in 5.8 fixed point: the power table's mantissa with its
/// implied bit, shifted down by the whole part.
pub(crate) fn attenuation_to_level(attenuation: u16) -> i32 {
    let mantissa = i32::from(POWER[usize::from(attenuation & 0xFF)] | 0x400) << 2;
    mantissa >> (attenuation >> 8).min(31)
}

/// Detune in phase-step units by key code and the low two bits of DT (Nemesis, t=386
/// start=165, p=6177, converted from the YM2608 manual's hertz).
const DETUNE: [[u8; 4]; 32] = [
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 1, 2, 2],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 5],
    [0, 2, 4, 5],
    [0, 2, 4, 6],
    [0, 2, 4, 6],
    [0, 2, 5, 7],
    [0, 2, 5, 8],
    [0, 3, 6, 8],
    [0, 3, 6, 9],
    [0, 3, 7, 10],
    [0, 4, 8, 11],
    [0, 4, 8, 12],
    [0, 4, 9, 13],
    [0, 5, 10, 14],
    [0, 5, 11, 16],
    [0, 6, 12, 17],
    [0, 6, 13, 19],
    [0, 7, 14, 20],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
];

/// What DT (3 bits) adds to a phase step at `key_code`: bit 2 is a sign, not two's
/// complement (YM2608 manual table 2-6; Nemesis, p=6177).
pub(crate) fn detune(key_code: u8, detune: u8) -> i32 {
    let value = i32::from(DETUNE[usize::from(key_code)][usize::from(detune & 0x03)]);
    if detune & 0x04 != 0 { -value } else { value }
}

/// The envelope's steps over eight updates in a row, for rates 0-1, 2-5 and 6-7 (Nemesis,
/// t=386 start=105, p=5716).
const SLOW_STEPS: [[u8; 8]; 3] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 0, 1, 0, 1, 0, 1],
    [0, 1, 1, 1, 0, 1, 1, 1],
];
/// The four patterns each group of four rates from 8 to 47 repeats (p=5716).
const STEPS: [[u8; 8]; 4] = [
    [0, 1, 0, 1, 0, 1, 0, 1],
    [0, 1, 0, 1, 1, 1, 0, 1],
    [0, 1, 1, 1, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 1],
];
/// Rates 48-51; each four rates from there double them, up to 8 from rate 60 (p=5716).
const FAST_STEPS: [[u8; 8]; 4] = [
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 2, 1, 1, 1, 2],
    [1, 2, 1, 2, 1, 2, 1, 2],
    [1, 2, 2, 2, 1, 2, 2, 2],
];

/// The step a `rate` (0-63) takes at envelope update `cycle`, if it moves then: rate r
/// moves every 2^(11 - r / 4) updates, every update from rate 44 on, and the step comes
/// from its pattern, read at the update's count above that shift (p=5716).
pub(crate) fn envelope_increment(rate: u8, cycle: u32) -> Option<i32> {
    let shift = 11u32.saturating_sub(u32::from(rate >> 2));
    if cycle & ((1 << shift) - 1) != 0 {
        return None;
    }
    let index = (cycle >> shift & 0x07) as usize;
    let step = match rate {
        0..=1 => SLOW_STEPS[0][index],
        2..=5 => SLOW_STEPS[1][index],
        6..=7 => SLOW_STEPS[2][index],
        8..=47 => STEPS[usize::from(rate & 0x03)][index],
        48..=59 => FAST_STEPS[usize::from(rate & 0x03)][index] << ((rate - 48) >> 2),
        _ => 8,
    };
    Some(i32::from(step))
}

/// Nemesis's phase modulation by depth (PMS) and LFO level (0-7), for frequency number
/// bit 9, in frequency-number units (t=386 start=480, p=8911); the other bits take it
/// doubled or halved per place.
const PHASE_MODULATION: [[u8; 8]; 8] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 1, 1, 1],
    [0, 0, 0, 1, 1, 1, 2, 2],
    [0, 0, 1, 1, 2, 2, 3, 3],
    [0, 0, 1, 2, 2, 2, 3, 4],
    [0, 0, 2, 3, 4, 4, 5, 6],
    [0, 0, 4, 6, 8, 8, 10, 12],
    [0, 0, 8, 12, 16, 16, 20, 24],
];

/// What the LFO adds to a frequency number in half units (the 12-bit number the phase
/// step starts from), from its top seven bits `top` at depth `sensitivity` and the LFO's
/// signed level `level`. Each table entry is a sum of at most two powers of two times the
/// bit-9 unit; the chip takes it as one or two shifted copies of `top`, added and scaled,
/// which drops their low bits separately [O].
pub(crate) fn phase_modulation(top: u32, sensitivity: u8, level: i8) -> i32 {
    let sensitivity = usize::from(sensitivity);
    // Depths 6 and 7 are depth 5 doubled and quadrupled.
    let scale = sensitivity.saturating_sub(5) as u32;
    let units =
        u32::from(PHASE_MODULATION[sensitivity][usize::from(level.unsigned_abs())]) >> scale;
    // Units of 4, 2 and 1 are `top` shifted by 0, 1 and 2 places.
    let copies: u32 = (0..3)
        .filter(|bit| units >> bit & 1 != 0)
        .map(|bit| top >> (2 - bit))
        .sum();
    let adjustment = ((copies << scale) >> 2) as i32;
    if level < 0 { -adjustment } else { adjustment }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_sine_and_power_tables_have_their_ends_and_bounds() {
        assert_eq!(LOG_SINE[0], 2137);
        assert_eq!(LOG_SINE[255], 0);
        assert!(LOG_SINE.windows(2).all(|pair| pair[0] >= pair[1]));
        assert_eq!(POWER[0], 1018);
        assert_eq!(POWER[255], 0);
    }

    #[test]
    fn test_sine_mirrors_its_quarter_and_turns_negative_in_the_second_half() {
        assert_eq!(sine(0), (LOG_SINE[0], false));
        assert_eq!(sine(0x1FF), (LOG_SINE[0], false));
        assert_eq!(sine(0x100), (LOG_SINE[255], false));
        assert_eq!(sine(0x200), (LOG_SINE[0], true));
        assert_eq!(sine(0x400), sine(0));
    }

    #[test]
    fn test_full_level_is_13_bits_and_each_256_halves_it() {
        assert_eq!(attenuation_to_level(0), 8168);
        assert_eq!(attenuation_to_level(0x100), 4084);
        assert_eq!(attenuation_to_level(13 << 8), 0);
    }

    #[test]
    fn test_detune_follows_nemesis_example_and_its_sign_bit() {
        assert_eq!(detune(0x14, 3), 0x0B);
        assert_eq!(detune(0x14, 7), -0x0B);
        assert_eq!(detune(0x1F, 4), 0);
    }

    #[test]
    fn test_envelope_rates_update_on_their_cycles() {
        assert_eq!(envelope_increment(0, 1), None);
        assert_eq!(envelope_increment(0, 2048), Some(0));
        assert_eq!(envelope_increment(4, 1024 * 3), Some(1));
        assert_eq!(envelope_increment(44, 5), Some(1));
        assert_eq!(envelope_increment(49, 3), Some(2));
        assert_eq!(envelope_increment(59, 4), Some(4));
        assert_eq!(envelope_increment(63, 1), Some(8));
    }

    #[test]
    fn test_phase_modulation_of_bit_9_is_nemesis_table_in_half_units() {
        for (sensitivity, row) in (0..8).zip(PHASE_MODULATION) {
            for (level, unit) in (0..8).zip(row) {
                let expected = 2 * i32::from(unit);
                assert_eq!(phase_modulation(1 << 5, sensitivity, level), expected);
                assert_eq!(phase_modulation(1 << 5, sensitivity, -level), -expected);
            }
        }
    }
}
