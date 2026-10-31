//! The builder's remembered settings: a small `key=value` file in the per-user folder the
//! game programs use for their controls (SDL's pref folder for AnyGenesis).

use std::{
    env, fs,
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "builder.txt";
const GAMES_FOLDER: &str = "AnyGenesis Games";

/// Light, dark, or whatever the system uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Reduce motion as chosen in Settings, or the system's choice when unset.
    pub reduce_motion: Option<bool>,
    /// Where games go, or the default folder when unset.
    pub games: Option<PathBuf>,
}

impl Settings {
    /// The settings in `text`, skipping lines it does not know.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            match key.trim() {
                "theme" => {
                    if let Some(theme) = ThemeChoice::ALL.into_iter().find(|t| t.key() == value) {
                        settings.theme = theme;
                    }
                }
                "reduce_motion" => settings.reduce_motion = value.parse().ok(),
                "games" if !value.is_empty() => settings.games = Some(PathBuf::from(value)),
                _ => {}
            }
        }
        settings
    }

    pub fn to_text(&self) -> String {
        let mut text = format!("theme={}\n", self.theme.key());
        if let Some(reduce) = self.reduce_motion {
            text.push_str(&format!("reduce_motion={reduce}\n"));
        }
        if let Some(games) = &self.games {
            text.push_str(&format!("games={}\n", games.display()));
        }
        text
    }

    /// The settings saved last time, or the defaults.
    pub fn load() -> Self {
        file()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Saves the settings; a folder that cannot be written only loses them.
    pub fn save(&self) {
        if let Some(path) = file() {
            if let Some(folder) = path.parent() {
                let _ = fs::create_dir_all(folder);
            }
            let _ = fs::write(path, self.to_text());
        }
    }

    /// Where games go.
    pub fn games_folder(&self) -> PathBuf {
        self.games.clone().unwrap_or_else(default_games_folder)
    }

    /// The games folder as the design shows it: "Documents › AnyGenesis Games" for the
    /// default, else the path.
    pub fn games_label(&self) -> String {
        match &self.games {
            Some(games) if *games != default_games_folder() => games.display().to_string(),
            _ => format!("Documents \u{203a} {GAMES_FOLDER}"),
        }
    }
}

/// The settings file: `%APPDATA%\AnyGenesis\AnyGenesis` on Windows, `$XDG_DATA_HOME/AnyGenesis`
/// (or `~/.local/share/AnyGenesis`) elsewhere, as SDL places the controls file.
fn file() -> Option<PathBuf> {
    let folder = if cfg!(windows) {
        PathBuf::from(env::var_os("APPDATA")?)
            .join("AnyGenesis")
            .join("AnyGenesis")
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home().map(|home| home.join(".local").join("share")))?
            .join("AnyGenesis")
    };
    Some(folder.join(FILE_NAME))
}

fn home() -> Option<PathBuf> {
    env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Documents/AnyGenesis Games: the Linux documents folder from `user-dirs.dirs` when set.
fn default_games_folder() -> PathBuf {
    let home = home().unwrap_or_default();
    let documents = if cfg!(windows) {
        None
    } else {
        xdg_documents(&home)
    };
    documents
        .unwrap_or_else(|| home.join("Documents"))
        .join(GAMES_FOLDER)
}

/// `XDG_DOCUMENTS_DIR` from `~/.config/user-dirs.dirs`, `$HOME` expanded.
fn xdg_documents(home: &Path) -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_documents(&text, home)
}

fn parse_documents(text: &str, home: &Path) -> Option<PathBuf> {
    let value = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("XDG_DOCUMENTS_DIR="))?
        .trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    (path != home && path.is_absolute()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_round_trip_through_text() {
        let settings = Settings {
            theme: ThemeChoice::Dark,
            reduce_motion: Some(true),
            games: Some(PathBuf::from("/games/here")),
        };
        assert_eq!(Settings::parse(&settings.to_text()), settings);
        assert_eq!(
            Settings::parse(&Settings::default().to_text()),
            Settings::default()
        );
    }

    #[test]
    fn test_settings_parse_skips_unknown_and_bad_lines() {
        let settings = Settings::parse("theme=purple\nreduce_motion=maybe\nnoise\nwho=me\n");
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn test_default_games_label_names_documents() {
        let settings = Settings::default();
        assert_eq!(
            settings.games_label(),
            "Documents \u{203a} AnyGenesis Games"
        );
        assert!(settings.games_folder().ends_with(GAMES_FOLDER));
    }

    #[cfg(unix)]
    #[test]
    fn test_xdg_documents_expands_home() {
        let home = Path::new("/home/me");
        let text = "# comment\nXDG_DOCUMENTS_DIR=\"$HOME/Dokumente\"\n";
        assert_eq!(
            parse_documents(text, home),
            Some(PathBuf::from("/home/me/Dokumente"))
        );
        assert_eq!(parse_documents("XDG_DOCUMENTS_DIR=\"$HOME/\"", home), None);
        assert_eq!(parse_documents("", home), None);
    }
}
