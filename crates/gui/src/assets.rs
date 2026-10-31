//! The bundled icons and app icon, embedded in the program and served to GPUI by path.

use std::borrow::Cow;

use anyhow::Result;
use gpui_kit::{AssetSource, SharedString};

macro_rules! embedded {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../assets/", $path)))),*]
    };
}

const FILES: &[(&str, &[u8])] = embedded![
    "icon.png",
    "icons/arrow-clockwise.svg",
    "icons/caret-down.svg",
    "icons/check-bold.svg",
    "icons/copy.svg",
    "icons/dots-six-vertical.svg",
    "icons/folder-open.svg",
    "icons/gear-six.svg",
    "icons/info.svg",
    "icons/play-fill.svg",
    "icons/puzzle-piece.svg",
    "icons/shield-warning.svg",
    "icons/trash.svg",
    "icons/upload-simple.svg",
    "icons/warning-circle.svg",
    "icons/warning.svg",
    "icons/x.svg",
];

/// IBM Plex Sans 400, 500 and 600 and Plex Mono 400.
pub const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
