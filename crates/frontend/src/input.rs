//! The keyboard and the first gamepad, mapped to the 3-button pad on port 1 by the
//! [`Controls`].

use anygenesis_runtime::Buttons;
use sdl3::{
    gamepad::{Axis, Button, Gamepad},
    keyboard::{KeyboardState, Scancode},
};

use crate::controls::{Controls, Modifiers};

/// How far, out of 32767, the left stick must lean to hold a direction.
const STICK_THRESHOLD: i16 = 16_384;

/// The pad buttons the keys in `held` hold. A key that completes a hotkey with modifiers
/// while they are held (Alt+Return) is the hotkey's, not the pad's, so the game does not
/// see it pressed with the hotkey.
pub fn keys(controls: &Controls, held: impl Fn(Scancode) -> bool) -> Buttons {
    let modifiers = Modifiers {
        ctrl: held(Scancode::LCtrl) || held(Scancode::RCtrl),
        alt: held(Scancode::LAlt) || held(Scancode::RAlt),
        shift: held(Scancode::LShift) || held(Scancode::RShift),
    };
    let in_hotkey = |key: Scancode| {
        controls.hotkeys.iter().any(|&(chord, _)| {
            chord.key == key
                && chord.modifiers != Modifiers::NONE
                && chord.modifiers.within(modifiers)
        })
    };
    collect(&controls.keys, |key| held(key) && !in_hotkey(key))
}

/// The pad buttons the gamepad's `held` buttons and its left stick at (`x`, `y`) hold; up
/// is negative `y`.
pub fn gamepad(controls: &Controls, held: impl Fn(Button) -> bool, x: i16, y: i16) -> Buttons {
    let stick = [
        (y <= -STICK_THRESHOLD, Buttons::UP),
        (y >= STICK_THRESHOLD, Buttons::DOWN),
        (x <= -STICK_THRESHOLD, Buttons::LEFT),
        (x >= STICK_THRESHOLD, Buttons::RIGHT),
    ];
    stick.into_iter().filter(|&(leaning, _)| leaning).fold(
        collect(&controls.buttons, held),
        |buttons, (_, direction)| buttons | direction,
    )
}

/// What the keyboard and `pad`, if one is open, hold together.
pub fn read(controls: &Controls, keyboard: &KeyboardState<'_>, pad: Option<&Gamepad>) -> Buttons {
    let from_keys = keys(controls, |scancode| keyboard.is_scancode_pressed(scancode));
    let Some(pad) = pad else {
        return from_keys;
    };
    from_keys
        | gamepad(
            controls,
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
    fn test_keys_map_arrows_zxc_and_return_by_default() {
        let controls = Controls::default();
        assert_eq!(keys(&controls, |_| false), Buttons::NONE);
        assert_eq!(
            keys(&controls, |key| key == Scancode::Return),
            Buttons::START
        );
        let held = [Scancode::Left, Scancode::Z, Scancode::C];
        assert_eq!(
            keys(&controls, |key| held.contains(&key)),
            Buttons::LEFT | Buttons::A | Buttons::C
        );
        assert_eq!(keys(&controls, |key| key == Scancode::X), Buttons::B);
        assert_eq!(keys(&controls, |key| key == Scancode::Space), Buttons::NONE);
    }

    #[test]
    fn test_keys_leave_a_held_modifier_hotkey_to_the_window() {
        let controls = Controls::default();
        let alt_return = [Scancode::RAlt, Scancode::Return, Scancode::Right];
        assert_eq!(
            keys(&controls, |key| alt_return.contains(&key)),
            Buttons::RIGHT
        );
        let ctrl_return = [Scancode::LCtrl, Scancode::Return];
        assert_eq!(
            keys(&controls, |key| ctrl_return.contains(&key)),
            Buttons::START
        );
    }

    #[test]
    fn test_keys_follow_the_controls_file() {
        let controls = Controls::parse("[keyboard]\nb = [\"Space\", \"X\"]\n").expect("parses");
        assert_eq!(keys(&controls, |key| key == Scancode::Space), Buttons::B);
        assert_eq!(keys(&controls, |key| key == Scancode::X), Buttons::B);
    }

    #[test]
    fn test_gamepad_maps_west_south_east_to_a_b_c_by_default() {
        let controls = Controls::default();
        let only = |wanted: Button| move |button| button == wanted;
        assert_eq!(gamepad(&controls, only(Button::West), 0, 0), Buttons::A);
        assert_eq!(gamepad(&controls, only(Button::South), 0, 0), Buttons::B);
        assert_eq!(gamepad(&controls, only(Button::East), 0, 0), Buttons::C);
        assert_eq!(gamepad(&controls, only(Button::North), 0, 0), Buttons::NONE);
        assert_eq!(
            gamepad(&controls, only(Button::Start), 0, 0),
            Buttons::START
        );
        assert_eq!(
            gamepad(&controls, only(Button::DPadDown), 0, 0),
            Buttons::DOWN
        );
    }

    #[test]
    fn test_gamepad_stick_holds_directions_past_half_way() {
        let controls = Controls::default();
        let none = |_| false;
        assert_eq!(gamepad(&controls, none, 16_383, -16_383), Buttons::NONE);
        assert_eq!(
            gamepad(&controls, none, STICK_THRESHOLD, -STICK_THRESHOLD),
            Buttons::RIGHT | Buttons::UP
        );
        assert_eq!(
            gamepad(&controls, none, i16::MIN, i16::MAX),
            Buttons::LEFT | Buttons::DOWN
        );
    }
}
