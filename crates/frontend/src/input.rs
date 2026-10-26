//! The keyboard and the first gamepad, mapped to the 3-button pad on port 1.

use anygenesis_runtime::Buttons;
use sdl3::{
    gamepad::{Axis, Button, Gamepad},
    keyboard::{KeyboardState, Scancode},
};

/// Keys and the pad buttons they hold.
const KEYS: [(Scancode, Buttons); 8] = [
    (Scancode::Up, Buttons::UP),
    (Scancode::Down, Buttons::DOWN),
    (Scancode::Left, Buttons::LEFT),
    (Scancode::Right, Buttons::RIGHT),
    (Scancode::Z, Buttons::A),
    (Scancode::X, Buttons::B),
    (Scancode::C, Buttons::C),
    (Scancode::Return, Buttons::START),
];

/// Gamepad buttons, by position, and the pad buttons they hold.
const PAD_BUTTONS: [(Button, Buttons); 8] = [
    (Button::DPadUp, Buttons::UP),
    (Button::DPadDown, Buttons::DOWN),
    (Button::DPadLeft, Buttons::LEFT),
    (Button::DPadRight, Buttons::RIGHT),
    (Button::West, Buttons::A),
    (Button::South, Buttons::B),
    (Button::East, Buttons::C),
    (Button::Start, Buttons::START),
];

/// How far, out of 32767, the left stick must lean to hold a direction.
const STICK_THRESHOLD: i16 = 16_384;

/// The pad buttons the keys in `held` hold.
pub fn keys(held: impl Fn(Scancode) -> bool) -> Buttons {
    collect(&KEYS, held)
}

/// The pad buttons the gamepad's `held` buttons and its left stick at (`x`, `y`) hold; up
/// is negative `y`.
pub fn gamepad(held: impl Fn(Button) -> bool, x: i16, y: i16) -> Buttons {
    let stick = [
        (y <= -STICK_THRESHOLD, Buttons::UP),
        (y >= STICK_THRESHOLD, Buttons::DOWN),
        (x <= -STICK_THRESHOLD, Buttons::LEFT),
        (x >= STICK_THRESHOLD, Buttons::RIGHT),
    ];
    stick
        .into_iter()
        .filter(|&(leaning, _)| leaning)
        .fold(collect(&PAD_BUTTONS, held), |buttons, (_, direction)| {
            buttons | direction
        })
}

/// What the keyboard and `pad`, if one is open, hold together.
pub fn read(keyboard: &KeyboardState<'_>, pad: Option<&Gamepad>) -> Buttons {
    let from_keys = keys(|scancode| keyboard.is_scancode_pressed(scancode));
    let Some(pad) = pad else {
        return from_keys;
    };
    from_keys
        | gamepad(
            |button| pad.button(button),
            pad.axis(Axis::LeftX),
            pad.axis(Axis::LeftY),
        )
}

/// The union of the pad buttons in `table` whose inputs are `held`.
fn collect<T: Copy>(table: &[(T, Buttons)], held: impl Fn(T) -> bool) -> Buttons {
    table
        .iter()
        .filter(|&&(input, _)| held(input))
        .fold(Buttons::NONE, |buttons, &(_, button)| buttons | button)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keys_map_arrows_zxc_and_return() {
        assert_eq!(keys(|_| false), Buttons::NONE);
        assert_eq!(keys(|key| key == Scancode::Return), Buttons::START);
        let held = [Scancode::Left, Scancode::Z, Scancode::C];
        assert_eq!(
            keys(|key| held.contains(&key)),
            Buttons::LEFT | Buttons::A | Buttons::C
        );
        assert_eq!(keys(|key| key == Scancode::X), Buttons::B);
        assert_eq!(keys(|key| key == Scancode::Space), Buttons::NONE);
    }

    #[test]
    fn test_gamepad_maps_west_south_east_to_a_b_c() {
        assert_eq!(gamepad(|button| button == Button::West, 0, 0), Buttons::A);
        assert_eq!(gamepad(|button| button == Button::South, 0, 0), Buttons::B);
        assert_eq!(gamepad(|button| button == Button::East, 0, 0), Buttons::C);
        assert_eq!(
            gamepad(|button| button == Button::North, 0, 0),
            Buttons::NONE
        );
        assert_eq!(
            gamepad(|button| button == Button::Start, 0, 0),
            Buttons::START
        );
        assert_eq!(
            gamepad(|button| button == Button::DPadDown, 0, 0),
            Buttons::DOWN
        );
    }

    #[test]
    fn test_gamepad_stick_holds_directions_past_half_way() {
        let none = |_| false;
        assert_eq!(gamepad(none, 16_383, -16_383), Buttons::NONE);
        assert_eq!(
            gamepad(none, STICK_THRESHOLD, -STICK_THRESHOLD),
            Buttons::RIGHT | Buttons::UP
        );
        assert_eq!(
            gamepad(none, i16::MIN, i16::MAX),
            Buttons::LEFT | Buttons::DOWN
        );
    }
}
