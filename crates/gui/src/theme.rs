//! The design's colour tokens (plan/design/design-tokens.json), light and dark.

use gpui_kit::{Hsla, rgb, rgba};

/// One set of colours from the design tokens.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Hsla,
    pub chrome: Hsla,
    pub surface: Hsla,
    pub border: Hsla,
    pub row: Hsla,
    pub window_border: Hsla,
    pub text: Hsla,
    pub text2: Hsla,
    pub muted: Hsla,
    pub control: Hsla,
    pub primary: Hsla,
    pub on_primary: Hsla,
    pub pill: Hsla,
    pub track: Hsla,
    pub log: Hsla,
    pub hover: Hsla,
    pub step_done: Hsla,
    pub on_step_done: Hsla,
    pub success: Hsla,
    pub info: Tone,
    pub warning: Tone,
    pub error: Tone,
    pub trust: Tone,
    pub scrim: Hsla,
    /// The hover colour of the window's close button.
    pub close_hover: Hsla,
}

/// The background, border and text colours of one kind of notice.
#[derive(Debug, Clone, Copy)]
pub struct Tone {
    pub bg: Hsla,
    pub border: Hsla,
    pub text: Hsla,
}

fn tone(bg: u32, border: u32, text: u32) -> Tone {
    Tone {
        bg: rgb(bg).into(),
        border: rgb(border).into(),
        text: rgb(text).into(),
    }
}

impl Theme {
    pub fn light() -> Self {
        Self {
            bg: rgb(0xF3F5F7).into(),
            chrome: rgb(0xEAEEF2).into(),
            surface: rgb(0xFFFFFF).into(),
            border: rgb(0xD9DEE4).into(),
            row: rgb(0xE6EAEE).into(),
            window_border: rgb(0xC3CAD2).into(),
            text: rgb(0x15191F).into(),
            text2: rgb(0x3E4650).into(),
            muted: rgb(0x59626E).into(),
            control: rgb(0x8A94A0).into(),
            primary: rgb(0x1769AA).into(),
            on_primary: rgb(0xFFFFFF).into(),
            pill: rgb(0xDCE8F3).into(),
            track: rgb(0xE1E6EB).into(),
            log: rgb(0xEEF2F5).into(),
            hover: rgb(0xEEF2F5).into(),
            step_done: rgb(0x59626E).into(),
            on_step_done: rgb(0xFFFFFF).into(),
            success: rgb(0x1D5E33).into(),
            info: tone(0xE6EEF6, 0xA9C6E2, 0x13507F),
            warning: tone(0xFFF4D6, 0xE8C66A, 0x5C4300),
            error: tone(0xFCE8E6, 0xE7A29B, 0x8A1F17),
            trust: tone(0xF0EBF8, 0xC5B4E3, 0x4B2C82),
            scrim: rgba(0x15191F52).into(),
            close_hover: rgb(0xC42B1C).into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: rgb(0x15191F).into(),
            chrome: rgb(0x1B2027).into(),
            surface: rgb(0x1D232B).into(),
            border: rgb(0x2E3640).into(),
            row: rgb(0x262D36).into(),
            window_border: rgb(0x3A434E).into(),
            text: rgb(0xE8ECF0).into(),
            text2: rgb(0xC3CAD2).into(),
            muted: rgb(0x9AA4AF).into(),
            control: rgb(0x6B7683).into(),
            primary: rgb(0x4BA3E3).into(),
            on_primary: rgb(0x0B1520).into(),
            pill: rgb(0x1F3447).into(),
            track: rgb(0x2A323C).into(),
            log: rgb(0x11151A).into(),
            hover: rgb(0x252C35).into(),
            step_done: rgb(0x9AA4AF).into(),
            on_step_done: rgb(0x15191F).into(),
            success: rgb(0xA6DDB7).into(),
            info: tone(0x1F3447, 0x2F587E, 0x9CCBF0),
            warning: tone(0x3A3015, 0x7A6420, 0xF2D88A),
            error: tone(0x3D1E1B, 0x7E3A33, 0xF4B4AC),
            trust: tone(0x2C2440, 0x574A80, 0xCDBDF2),
            scrim: rgba(0x00000080).into(),
            close_hover: rgb(0xC42B1C).into(),
        }
    }
}
