//! Where the Marketplace reads from: meh7an/AnyGenesis-Patches over HTTPS, the index from
//! `main` and every file from the commit the index names, or a local copy of the repository
//! named by `ANYGENESIS_MARKET`, for development.

use std::{env, path::PathBuf, time::Duration};

use anygenesis_patch::market::{LocalSource, MAX_FILE_SIZE, Source};
use ureq::Agent;

const INDEX_URL: &str =
    "https://raw.githubusercontent.com/meh7an/AnyGenesis-Patches/main/index.toml";
const FILES_URL: &str = "https://raw.githubusercontent.com/meh7an/AnyGenesis-Patches/";
const TIMEOUT: Duration = Duration::from_secs(20);

/// The Marketplace's source for this run.
pub fn source() -> Box<dyn Source + Send> {
    match env::var_os("ANYGENESIS_MARKET") {
        Some(root) => Box::new(LocalSource {
            root: PathBuf::from(root),
        }),
        None => Box::new(Github::new()),
    }
}

/// The repository on GitHub, HTTPS only, redirects included.
struct Github {
    agent: Agent,
}

impl Github {
    fn new() -> Self {
        let config = Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(TIMEOUT))
            .build();
        Self {
            agent: Agent::new_with_config(config),
        }
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|error| error.to_string())?;
        // ureq fails a body of exactly its limit (the read that would see the end finds no
        // room left), so allow one byte more; the install refuses any size but the index's.
        response
            .body_mut()
            .with_config()
            .limit(limit.saturating_add(1))
            .read_to_vec()
            .map_err(|error| error.to_string())
    }
}

impl Source for Github {
    fn index(&self) -> Result<Vec<u8>, String> {
        self.get(INDEX_URL, MAX_FILE_SIZE)
    }

    fn file(&self, commit: &str, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        // Index paths hold only ASCII letters, digits, `-`, `_`, `.` and spaces, so a space
        // is the one character to escape.
        let url = format!("{FILES_URL}{commit}/{}", path.replace(' ', "%20"));
        self.get(&url, limit)
    }
}
