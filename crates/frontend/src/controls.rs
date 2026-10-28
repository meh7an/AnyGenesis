//! What the keyboard and gamepad do in the window, read from a TOML file in SDL's key and
//! button names.
//!
//! ```toml
//! [keyboard]          # pad 1: up down left right a b c start
//! a = ["Z", "A"]
//! [gamepad]           # SDL gamepad names by position: a = bottom, x = left, ...
//! c = ["b"]
//! [hotkeys]           # fullscreen scale pause reset quit; Ctrl+, Alt+, Shift+ may lead a key
//! fullscreen = ["F11", "Alt+Return"]
//! scale = ["F10"]     # exact, fit, stretch in turn; see settings
//! [gamepad_hotkeys]
//! pause = ["back"]
//! [window]
//! fullscreen = false          # start in fullscreen
//! pause_on_focus_loss = false
//! ```
//!
//! Whatever the file leaves out keeps its default and an empty list unbinds an action. Names
//! are case-insensitive. A key or button may hold several pad buttons, but nothing may be both
//! a pad button and a hotkey, or two hotkeys.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anygenesis_runtime::Buttons;
use sdl3::{
    gamepad::Button,
    keyboard::{Mod, Scancode},
};
use serde::Deserialize;
use toml::Spanned;

use crate::{Error, sdl};

/// The folder SDL keeps per-user files in, by organisation and application.
const PREF_FOLDER: (&str, &str) = ("AnyGenesis", "AnyGenesis");

/// The controls file's name in that folder.
const FILE_NAME: &str = "controls.toml";

/// The pad buttons by the names the file gives them.
const PAD_NAMES: [(&str, Buttons); 8] = [
    ("up", Buttons::UP),
    ("down", Buttons::DOWN),
    ("left", Buttons::LEFT),
    ("right", Buttons::RIGHT),
    ("a", Buttons::A),
    ("b", Buttons::B),
    ("c", Buttons::C),
    ("start", Buttons::START),
];

/// Modifier prefixes a hotkey may carry, as written before its key.
const MODIFIER_PREFIXES: [(&str, Modifiers); 3] = [
    ("ctrl+", Modifiers::CTRL),
    ("alt+", Modifiers::ALT),
    ("shift+", Modifiers::SHIFT),
];

/// What a hotkey does to the window or the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hotkey {
    Fullscreen,
    /// The next picture scaling, remembered for later runs.
    Scale,
    Pause,
    Reset,
    Quit,
}

impl Hotkey {
    const ALL: [Self; 5] = [
        Self::Fullscreen,
        Self::Scale,
        Self::Pause,
        Self::Reset,
        Self::Quit,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Fullscreen => "fullscreen",
            Self::Scale => "scale",
            Self::Pause => "pause",
            Self::Reset => "reset",
            Self::Quit => "quit",
        }
    }
}

/// Which of Ctrl, Alt and Shift are held, either side counting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };
    const CTRL: Self = Self {
        ctrl: true,
        ..Self::NONE
    };
    const ALT: Self = Self {
        alt: true,
        ..Self::NONE
    };
    const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };

    /// The modifiers SDL reports as held in `keymod`.
    pub fn held(keymod: Mod) -> Self {
        Self {
            ctrl: keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD),
            alt: keymod.intersects(Mod::LALTMOD | Mod::RALTMOD),
            shift: keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD),
        }
    }

    /// Whether every modifier in `self` is also in `held`.
    pub fn within(self, held: Self) -> bool {
        (!self.ctrl || held.ctrl) && (!self.alt || held.alt) && (!self.shift || held.shift)
    }

    fn with(self, other: Self) -> Self {
        Self {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
        }
    }
}

/// A key pressed with exactly these modifiers held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub key: Scancode,
    pub modifiers: Modifiers,
}

impl Chord {
    const fn plain(key: Scancode) -> Self {
        Self {
            key,
            modifiers: Modifiers::NONE,
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Modifiers { ctrl, alt, shift } = self.modifiers;
        for (held, name) in [(ctrl, "Ctrl+"), (alt, "Alt+"), (shift, "Shift+")] {
            if held {
                f.write_str(name)?;
            }
        }
        f.write_str(self.key.name())
    }
}

/// The keyboard and gamepad bindings and how the window starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controls {
    /// Keys and the pad buttons they hold.
    pub keys: Vec<(Scancode, Buttons)>,
    /// Gamepad buttons, by position, and the pad buttons they hold; the left stick always
    /// holds the directions too.
    pub buttons: Vec<(Button, Buttons)>,
    pub hotkeys: Vec<(Chord, Hotkey)>,
    pub button_hotkeys: Vec<(Button, Hotkey)>,
    /// Whether the window opens in fullscreen.
    pub fullscreen: bool,
    /// Whether the run waits while the window does not have the keyboard.
    pub pause_on_focus_loss: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            keys: vec![
                (Scancode::Up, Buttons::UP),
                (Scancode::Down, Buttons::DOWN),
                (Scancode::Left, Buttons::LEFT),
                (Scancode::Right, Buttons::RIGHT),
                (Scancode::Z, Buttons::A),
                (Scancode::X, Buttons::B),
                (Scancode::C, Buttons::C),
                (Scancode::Return, Buttons::START),
            ],
            buttons: vec![
                (Button::DPadUp, Buttons::UP),
                (Button::DPadDown, Buttons::DOWN),
                (Button::DPadLeft, Buttons::LEFT),
                (Button::DPadRight, Buttons::RIGHT),
                (Button::West, Buttons::A),
                (Button::South, Buttons::B),
                (Button::East, Buttons::C),
                (Button::Start, Buttons::START),
            ],
            hotkeys: vec![
                (Chord::plain(Scancode::F11), Hotkey::Fullscreen),
                (
                    Chord {
                        key: Scancode::Return,
                        modifiers: Modifiers::ALT,
                    },
                    Hotkey::Fullscreen,
                ),
                (Chord::plain(Scancode::F10), Hotkey::Scale),
                (Chord::plain(Scancode::P), Hotkey::Pause),
                (Chord::plain(Scancode::F5), Hotkey::Reset),
                (Chord::plain(Scancode::Escape), Hotkey::Quit),
            ],
            button_hotkeys: vec![(Button::Back, Hotkey::Pause)],
            fullscreen: false,
            pause_on_focus_loss: false,
        }
    }
}

impl Controls {
    /// Reads the controls file at `path`; `None` when there is no file there.
    ///
    /// # Errors
    /// The file cannot be read, is not TOML of this shape, or binds something wrongly.
    pub fn load(path: &Path) -> Result<Option<Self>, ControlsError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ControlsError::Read(error)),
        }
    }

    /// The controls `text` describes, over the defaults.
    ///
    /// # Errors
    /// `text` is not TOML of this shape, or binds something wrongly; every problem found is
    /// listed.
    pub fn parse(text: &str) -> Result<Self, ControlsError> {
        let raw: RawControls = toml::from_str(text).map_err(|error| ControlsError::Parse {
            line: error.span().map(|span| line_of(text, span.start)),
            message: error.message().to_owned(),
        })?;
        let mut validator = Validator {
            text,
            problems: Vec::new(),
        };
        let controls = validator.controls(raw);
        if validator.problems.is_empty() {
            Ok(controls)
        } else {
            Err(ControlsError::Invalid {
                problems: validator.problems,
            })
        }
    }

    /// What `chord` does, if it is a hotkey.
    pub fn hotkey(&self, chord: Chord) -> Option<Hotkey> {
        find(&self.hotkeys, chord)
    }

    /// What gamepad `button` does, if it is a hotkey.
    pub fn button_hotkey(&self, button: Button) -> Option<Hotkey> {
        find(&self.button_hotkeys, button)
    }
}

/// Where the controls file lives unless another is named: SDL's per-user folder for the
/// application, which SDL creates if it is missing.
///
/// # Errors
/// SDL finding no such folder.
pub fn default_path() -> Result<PathBuf, Error> {
    let (organisation, application) = PREF_FOLDER;
    let folder = sdl3::filesystem::get_pref_path(organisation, application).map_err(sdl)?;
    Ok(folder.join(FILE_NAME))
}

/// Why a controls file was refused.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ControlsError {
    #[error("reading the file: {0}")]
    Read(io::Error),
    /// The text is not TOML, or does not fit the file's sections, names and types.
    #[error("{}", Located { line: *line, message })]
    Parse {
        /// The line the TOML parser stopped at, when it says.
        line: Option<usize>,
        message: String,
    },
    /// The bindings break a rule; every problem found is listed.
    #[error("{}", ProblemList(problems))]
    Invalid { problems: Vec<Problem> },
}

/// One binding the file gets wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The line of the offending name; none when it clashes only with defaults.
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let located = Located {
            line: self.line,
            message: &self.message,
        };
        write!(f, "{located}")
    }
}

struct Located<'a> {
    line: Option<usize>,
    message: &'a str,
}

impl fmt::Display for Located<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(self.message),
        }
    }
}

struct ProblemList<'a>(&'a [Problem]);

impl fmt::Display for ProblemList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, problem) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{problem}")?;
        }
        Ok(())
    }
}

/// Names as written, each with its byte span.
type Names = Vec<Spanned<String>>;

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawControls {
    #[serde(default)]
    keyboard: Option<RawPad>,
    #[serde(default)]
    gamepad: Option<RawPad>,
    #[serde(default)]
    hotkeys: Option<RawHotkeys>,
    #[serde(default)]
    gamepad_hotkeys: Option<RawHotkeys>,
    #[serde(default)]
    window: Option<RawWindow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPad {
    up: Option<Names>,
    down: Option<Names>,
    left: Option<Names>,
    right: Option<Names>,
    a: Option<Names>,
    b: Option<Names>,
    c: Option<Names>,
    start: Option<Names>,
}

impl RawPad {
    /// Each pad button with the names bound to it, if the file lists any, in
    /// [`PAD_NAMES`] order.
    fn entries(self) -> [(Buttons, Option<Names>); 8] {
        let Self {
            up,
            down,
            left,
            right,
            a,
            b,
            c,
            start,
        } = self;
        let mut names = [up, down, left, right, a, b, c, start].into_iter();
        PAD_NAMES.map(|(_, buttons)| (buttons, names.next().flatten()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHotkeys {
    fullscreen: Option<Names>,
    scale: Option<Names>,
    pause: Option<Names>,
    reset: Option<Names>,
    quit: Option<Names>,
}

impl RawHotkeys {
    /// Each hotkey with the names bound to it, if the file lists any, in [`Hotkey::ALL`]
    /// order.
    fn entries(self) -> [(Hotkey, Option<Names>); 5] {
        let Self {
            fullscreen,
            scale,
            pause,
            reset,
            quit,
        } = self;
        let mut names = [fullscreen, scale, pause, reset, quit].into_iter();
        Hotkey::ALL.map(|hotkey| (hotkey, names.next().flatten()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWindow {
    fullscreen: Option<bool>,
    pause_on_focus_loss: Option<bool>,
}

/// A binding with the line it was written on; none for a default.
#[derive(Debug, Clone, Copy)]
struct Bound<I, A> {
    input: I,
    action: A,
    line: Option<usize>,
}

/// Turns the raw file into controls, collecting every problem on the way.
struct Validator<'a> {
    text: &'a str,
    problems: Vec<Problem>,
}

impl Validator<'_> {
    fn controls(&mut self, raw: RawControls) -> Controls {
        let RawControls {
            keyboard,
            gamepad,
            hotkeys,
            gamepad_hotkeys,
            window,
        } = raw;
        let defaults = Controls::default();
        let keys = self.bind(
            keyboard.map(RawPad::entries),
            &defaults.keys,
            Self::plain_key,
        );
        let buttons = self.bind(
            gamepad.map(RawPad::entries),
            &defaults.buttons,
            Self::button,
        );
        let hotkeys = self.bind(
            hotkeys.map(RawHotkeys::entries),
            &defaults.hotkeys,
            Self::chord,
        );
        let button_hotkeys = self.bind(
            gamepad_hotkeys.map(RawHotkeys::entries),
            &defaults.button_hotkeys,
            Self::button,
        );
        let pad_keys: Vec<_> = keys
            .iter()
            .map(|bound| Bound {
                input: Chord::plain(bound.input),
                action: bound.action,
                line: bound.line,
            })
            .collect();
        self.clashes(&pad_keys, &hotkeys);
        self.clashes(&buttons, &button_hotkeys);
        let window = window.unwrap_or(RawWindow {
            fullscreen: None,
            pause_on_focus_loss: None,
        });
        Controls {
            keys: unbound(keys),
            buttons: unbound(buttons),
            hotkeys: unbound(hotkeys),
            button_hotkeys: unbound(button_hotkeys),
            fullscreen: window.fullscreen.unwrap_or(defaults.fullscreen),
            pause_on_focus_loss: window
                .pause_on_focus_loss
                .unwrap_or(defaults.pause_on_focus_loss),
        }
    }

    /// The bindings of one section: for each action the names the file lists, else its
    /// defaults. Names that do not parse are reported and left out.
    fn bind<I: Copy, A: Copy + PartialEq, const N: usize>(
        &mut self,
        entries: Option<[(A, Option<Names>); N]>,
        defaults: &[(I, A)],
        parse: fn(&mut Self, &Spanned<String>) -> Option<I>,
    ) -> Vec<Bound<I, A>> {
        let default = |&(input, action): &(I, A)| Bound {
            input,
            action,
            line: None,
        };
        let Some(entries) = entries else {
            return defaults.iter().map(default).collect();
        };
        let mut bindings = Vec::new();
        for (action, names) in entries {
            let Some(names) = names else {
                let of_action = defaults.iter().filter(|&&(_, bound)| bound == action);
                bindings.extend(of_action.map(default));
                continue;
            };
            for name in &names {
                if let Some(input) = parse(self, name) {
                    bindings.push(Bound {
                        input,
                        action,
                        line: Some(self.line(name)),
                    });
                }
            }
        }
        bindings
    }

    /// Reports every input in `pad` that is also a hotkey, and every input bound to two
    /// different hotkeys.
    fn clashes<I: Copy + PartialEq + Describe>(
        &mut self,
        pad: &[Bound<I, Buttons>],
        hotkeys: &[Bound<I, Hotkey>],
    ) {
        for (index, hotkey) in hotkeys.iter().enumerate() {
            if let Some(button) = pad.iter().find(|bound| bound.input == hotkey.input) {
                self.problems.push(Problem {
                    line: hotkey.line.or(button.line),
                    message: format!(
                        "{} is both pad button {} and hotkey {}",
                        hotkey.input.describe(),
                        pad_name(button.action),
                        hotkey.action.name()
                    ),
                });
            }
            let earlier = hotkeys[..index]
                .iter()
                .find(|other| other.input == hotkey.input && other.action != hotkey.action);
            if let Some(other) = earlier {
                self.problems.push(Problem {
                    line: hotkey.line.or(other.line),
                    message: format!(
                        "{} is both hotkey {} and hotkey {}",
                        hotkey.input.describe(),
                        other.action.name(),
                        hotkey.action.name()
                    ),
                });
            }
        }
    }

    /// A key for a pad button, which takes no modifiers.
    fn plain_key(&mut self, name: &Spanned<String>) -> Option<Scancode> {
        let chord = self.chord(name)?;
        if chord.modifiers != Modifiers::NONE {
            let message = format!(
                "pad buttons take a key without modifiers, not {:?}",
                name.get_ref()
            );
            self.report(name, message);
            return None;
        }
        Some(chord.key)
    }

    fn chord(&mut self, name: &Spanned<String>) -> Option<Chord> {
        let mut rest = name.get_ref().as_str();
        let mut modifiers = Modifiers::NONE;
        while let Some((prefix, modifier)) = MODIFIER_PREFIXES.iter().find(|(prefix, _)| {
            rest.get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        }) {
            rest = &rest[prefix.len()..];
            modifiers = modifiers.with(*modifier);
        }
        match Scancode::from_name(rest) {
            Some(key) => Some(Chord { key, modifiers }),
            None => {
                self.report(name, format!("{:?} is not an SDL key name", name.get_ref()));
                None
            }
        }
    }

    fn button(&mut self, name: &Spanned<String>) -> Option<Button> {
        let button = Button::from_string(name.get_ref());
        if button.is_none() {
            self.report(
                name,
                format!("{:?} is not an SDL gamepad button name", name.get_ref()),
            );
        }
        button
    }

    fn line(&self, name: &Spanned<String>) -> usize {
        line_of(self.text, name.span().start)
    }

    fn report(&mut self, name: &Spanned<String>, message: String) {
        self.problems.push(Problem {
            line: Some(self.line(name)),
            message,
        });
    }
}

/// How an input reads in a message.
trait Describe {
    fn describe(self) -> String;
}

impl Describe for Chord {
    fn describe(self) -> String {
        self.to_string()
    }
}

impl Describe for Button {
    fn describe(self) -> String {
        format!("gamepad {}", self.string())
    }
}

/// The bindings without their lines.
fn unbound<I, A>(bindings: Vec<Bound<I, A>>) -> Vec<(I, A)> {
    bindings
        .into_iter()
        .map(|bound| (bound.input, bound.action))
        .collect()
}

/// The action bound to `input` in `table`, if any.
fn find<I: PartialEq, A: Copy>(table: &[(I, A)], input: I) -> Option<A> {
    table
        .iter()
        .find(|(bound, _)| *bound == input)
        .map(|&(_, action)| action)
}

/// The name the file gives pad button `buttons`.
fn pad_name(buttons: Buttons) -> &'static str {
    PAD_NAMES
        .iter()
        .find(|&&(_, named)| named == buttons)
        .map_or("?", |&(name, _)| name)
}

/// The 1-based line of byte `offset` in `text`.
fn line_of(text: &str, offset: usize) -> usize {
    let before = text.get(..offset).unwrap_or(text);
    before.matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problems(text: &str) -> Vec<Problem> {
        match Controls::parse(text) {
            Err(ControlsError::Invalid { problems }) => problems,
            other => panic!("expected problems, got {other:?}"),
        }
    }

    fn problem(line: usize, message: &str) -> Problem {
        Problem {
            line: Some(line),
            message: message.to_owned(),
        }
    }

    #[test]
    fn test_parse_empty_file_gives_the_defaults() {
        assert_eq!(Controls::parse("").expect("parses"), Controls::default());
    }

    #[test]
    fn test_parse_listed_action_replaces_only_its_defaults() {
        let controls = Controls::parse("[keyboard]\na = [\"A\", \"space\"]\n").expect("parses");
        let a_keys: Vec<Scancode> = controls
            .keys
            .iter()
            .filter(|&&(_, buttons)| buttons == Buttons::A)
            .map(|&(key, _)| key)
            .collect();
        assert_eq!(a_keys, [Scancode::A, Scancode::Space]);
        assert!(controls.keys.contains(&(Scancode::X, Buttons::B)));
        assert!(!controls.keys.contains(&(Scancode::Z, Buttons::A)));
        assert_eq!(controls.hotkeys, Controls::default().hotkeys);
    }

    #[test]
    fn test_parse_scale_hotkey_defaults_to_f10_and_rebinds() {
        let defaults = Controls::default();
        assert_eq!(
            defaults.hotkey(Chord::plain(Scancode::F10)),
            Some(Hotkey::Scale)
        );
        let controls = Controls::parse("[hotkeys]\nscale = [\"F9\"]\n").expect("parses");
        assert_eq!(controls.hotkey(Chord::plain(Scancode::F10)), None);
        assert_eq!(
            controls.hotkey(Chord::plain(Scancode::F9)),
            Some(Hotkey::Scale)
        );
    }

    #[test]
    fn test_parse_empty_list_unbinds_the_action() {
        let controls = Controls::parse("[hotkeys]\nreset = []\n").expect("parses");
        assert_eq!(controls.hotkey(Chord::plain(Scancode::F5)), None);
        assert_eq!(
            controls.hotkey(Chord::plain(Scancode::P)),
            Some(Hotkey::Pause)
        );
    }

    #[test]
    fn test_parse_gamepad_names_are_sdl_positions() {
        let text = "[gamepad]\na = [\"x\"]\nb = [\"A\"]\nc = [\"b\"]\n\
                    [gamepad_hotkeys]\npause = []\nreset = [\"guide\"]\n";
        let controls = Controls::parse(text).expect("parses");
        assert!(controls.buttons.contains(&(Button::West, Buttons::A)));
        assert!(controls.buttons.contains(&(Button::South, Buttons::B)));
        assert!(controls.buttons.contains(&(Button::East, Buttons::C)));
        assert_eq!(controls.button_hotkey(Button::Back), None);
        assert_eq!(controls.button_hotkey(Button::Guide), Some(Hotkey::Reset));
    }

    #[test]
    fn test_parse_hotkeys_take_modifiers_in_any_case_and_order() {
        let text = "[hotkeys]\nquit = [\"shift+CTRL+q\", \"Alt+Keypad +\"]\n";
        let controls = Controls::parse(text).expect("parses");
        let chord = Chord {
            key: Scancode::Q,
            modifiers: Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            },
        };
        assert_eq!(controls.hotkey(chord), Some(Hotkey::Quit));
        let keypad = Chord {
            key: Scancode::KpPlus,
            modifiers: Modifiers::ALT,
        };
        assert_eq!(controls.hotkey(keypad), Some(Hotkey::Quit));
        assert_eq!(controls.hotkey(Chord::plain(Scancode::Q)), None);
        assert_eq!(controls.hotkey(Chord::plain(Scancode::Escape)), None);
    }

    #[test]
    fn test_parse_window_section_sets_the_start() {
        let text = "[window]\nfullscreen = true\npause_on_focus_loss = true\n";
        let controls = Controls::parse(text).expect("parses");
        assert!(controls.fullscreen);
        assert!(controls.pause_on_focus_loss);
        assert_eq!(controls.keys, Controls::default().keys);
    }

    #[test]
    fn test_parse_bad_toml_reports_its_line() {
        let error = Controls::parse("[keyboard]\na = [\"Z\" \"X\"]\n").expect_err("refused");
        assert!(
            matches!(error, ControlsError::Parse { line: Some(2), .. }),
            "{error:?}"
        );
    }

    #[test]
    fn test_parse_unknown_section_or_action_reports_its_line() {
        for text in ["[mouse]\n", "[keyboard]\njump = [\"Z\"]\n"] {
            let error = Controls::parse(text).expect_err("refused");
            let ControlsError::Parse { line, message } = &error else {
                panic!("{error:?}");
            };
            assert_eq!(*line, Some(text.lines().count()), "{text}");
            assert!(message.contains("unknown field"), "{message}");
        }
    }

    #[test]
    fn test_parse_wrong_type_is_refused() {
        let error = Controls::parse("[keyboard]\na = \"Z\"\n").expect_err("refused");
        assert!(matches!(error, ControlsError::Parse { line: Some(2), .. }));
    }

    #[test]
    fn test_parse_unknown_names_are_each_reported_with_their_line() {
        let text = "[keyboard]\na = [\"Z\", \"Zed\"]\n[gamepad]\nb = [\"cross\"]\n\
                    [hotkeys]\npause = [\"Hyper+P\"]\n";
        assert_eq!(
            problems(text),
            [
                problem(2, "\"Zed\" is not an SDL key name"),
                problem(4, "\"cross\" is not an SDL gamepad button name"),
                problem(6, "\"Hyper+P\" is not an SDL key name"),
            ]
        );
    }

    #[test]
    fn test_parse_pad_key_with_modifier_is_refused() {
        assert_eq!(
            problems("[keyboard]\nstart = [\"Alt+Return\"]\n"),
            [problem(
                2,
                "pad buttons take a key without modifiers, not \"Alt+Return\""
            )]
        );
    }

    #[test]
    fn test_parse_key_on_a_pad_button_and_a_hotkey_is_refused() {
        assert_eq!(
            problems("[hotkeys]\npause = [\"Return\"]\n"),
            [problem(
                2,
                "Return is both pad button start and hotkey pause"
            )]
        );
        assert_eq!(
            problems("[keyboard]\nstart = [\"F5\"]\n"),
            [problem(2, "F5 is both pad button start and hotkey reset")]
        );
        assert_eq!(
            problems("[gamepad]\nc = [\"back\"]\n"),
            [problem(
                2,
                "gamepad back is both pad button c and hotkey pause"
            )]
        );
    }

    #[test]
    fn test_parse_modified_hotkey_may_share_a_pad_key() {
        let controls = Controls::parse("[hotkeys]\nreset = [\"Ctrl+Z\"]\n").expect("parses");
        assert!(controls.keys.contains(&(Scancode::Z, Buttons::A)));
    }

    #[test]
    fn test_parse_input_on_two_hotkeys_is_refused() {
        assert_eq!(
            problems("[hotkeys]\nquit = [\"P\"]\n"),
            [problem(2, "P is both hotkey pause and hotkey quit")]
        );
        assert_eq!(
            problems("[gamepad_hotkeys]\nreset = [\"back\"]\n"),
            [problem(
                2,
                "gamepad back is both hotkey pause and hotkey reset"
            )]
        );
    }

    #[test]
    fn test_problems_display_one_per_line() {
        let error = ControlsError::Invalid {
            problems: vec![
                problem(2, "first"),
                Problem {
                    line: None,
                    message: "second".to_owned(),
                },
            ],
        };
        assert_eq!(error.to_string(), "line 2: first\nsecond");
    }

    #[test]
    fn test_load_missing_file_is_none() {
        let path = Path::new("no such folder").join(FILE_NAME);
        assert!(Controls::load(&path).expect("no error").is_none());
    }

    #[test]
    fn test_modifiers_held_counts_either_side() {
        assert_eq!(Modifiers::held(Mod::RALTMOD), Modifiers::ALT);
        assert_eq!(
            Modifiers::held(Mod::LCTRLMOD | Mod::RSHIFTMOD | Mod::NUMMOD),
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            }
        );
    }

    #[test]
    fn test_chord_displays_like_the_file_writes_it() {
        let chord = Chord {
            key: Scancode::Return,
            modifiers: Modifiers::ALT,
        };
        assert_eq!(chord.to_string(), "Alt+Return");
    }
}
