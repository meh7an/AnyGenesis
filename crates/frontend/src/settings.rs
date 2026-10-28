//! What the window remembers between runs, in a TOML file it writes itself next to the
//! controls file.
//!
//! ```toml
//! scaling = "exact"   # exact, fit or stretch
//! ```
//!
//! The file is the program's, not the player's: a missing or unreadable one gives the
//! defaults, and a change in the window writes it anew.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Error, controls};

/// The settings file's name in the controls file's folder.
const FILE_NAME: &str = "settings.toml";

/// How the picture fills the window or the screen; pixels stay sharp in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Scaling {
    /// The largest whole-number scale that fits, so every picture pixel is the same size.
    #[default]
    Exact,
    /// As large as fits with the picture's shape kept; pixels may differ by one in size.
    Fit,
    /// The whole window or screen, the picture's shape not kept.
    Stretch,
}

impl Scaling {
    /// The mode after `self`, back to the first after the last.
    pub fn next(self) -> Self {
        match self {
            Self::Exact => Self::Fit,
            Self::Fit => Self::Stretch,
            Self::Stretch => Self::Exact,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fit => "fit",
            Self::Stretch => "stretch",
        }
    }
}

/// What the window remembers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub scaling: Scaling,
}

impl Settings {
    /// Reads the settings file at `path`; `None` when there is no file there.
    ///
    /// # Errors
    /// The file cannot be read or is not TOML of this shape.
    pub fn load(path: &Path) -> Result<Option<Self>, SettingsError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SettingsError::Read(error)),
        }
    }

    /// The settings `text` describes, over the defaults.
    ///
    /// # Errors
    /// `text` is not TOML of this shape.
    pub fn parse(text: &str) -> Result<Self, SettingsError> {
        toml::from_str(text).map_err(|error| SettingsError::Parse(error.message().to_owned()))
    }

    /// Writes the settings to `path`, replacing what was there.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        fs::write(path, self.to_text()).map_err(SettingsError::Write)
    }

    /// The settings as the file holds them.
    fn to_text(self) -> String {
        // Infallible: a struct of unit enum variants always serialises to a TOML table.
        toml::to_string(&self).expect("settings serialise to TOML")
    }
}

/// Where the settings file lives: the folder of the default controls file.
///
/// # Errors
/// SDL finding no such folder.
pub fn default_path() -> Result<PathBuf, Error> {
    Ok(controls::default_path()?.with_file_name(FILE_NAME))
}

/// Why the settings file could not be used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    #[error("reading the file: {0}")]
    Read(io::Error),
    #[error("{0}")]
    Parse(String),
    #[error("writing the file: {0}")]
    Write(io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scaling_next_cycles_exact_fit_stretch() {
        assert_eq!(Scaling::Exact.next(), Scaling::Fit);
        assert_eq!(Scaling::Fit.next(), Scaling::Stretch);
        assert_eq!(Scaling::Stretch.next(), Scaling::Exact);
    }

    #[test]
    fn test_parse_empty_file_gives_the_defaults() {
        let settings = Settings::parse("").expect("parses");
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.scaling, Scaling::Exact);
    }

    #[test]
    fn test_parse_reads_each_scaling_by_name() {
        for scaling in [Scaling::Exact, Scaling::Fit, Scaling::Stretch] {
            let text = format!("scaling = \"{}\"\n", scaling.name());
            assert_eq!(Settings::parse(&text).expect("parses").scaling, scaling);
        }
    }

    #[test]
    fn test_parse_unknown_scaling_or_key_is_refused() {
        for text in ["scaling = \"zoom\"\n", "scale = \"fit\"\n"] {
            assert!(
                matches!(Settings::parse(text), Err(SettingsError::Parse(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn test_to_text_parses_back_to_the_same_settings() {
        let settings = Settings {
            scaling: Scaling::Stretch,
        };
        assert_eq!(settings.to_text(), "scaling = \"stretch\"\n");
        assert_eq!(
            Settings::parse(&settings.to_text()).expect("parses"),
            settings
        );
    }

    #[test]
    fn test_load_missing_file_is_none() {
        let path = Path::new("no such folder").join(FILE_NAME);
        assert!(Settings::load(&path).expect("no error").is_none());
    }
}
