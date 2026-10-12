use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::paths;

/// User settings, stored as `config.toml` in the data directory.
///
/// Every field has a default, so an absent or partial file is valid. Command
/// line flags win over the file.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// `en-US` or `ru-RU`; affects messages only, not manifest fetching.
    pub locale: Option<String>,
    /// `auto` (default when a terminal is attached), `always`, `never`.
    pub color: Option<String>,
    /// Warm manifest cache for the top search hits in interactive terminals.
    pub prefetch: Option<bool>,
    /// Run EXE installers without asking.
    pub trust_exe_installers: Option<bool>,
    /// Only install from these publishers (empty means no restriction).
    pub trusted_publishers: Option<Vec<String>>,
    /// Verify the index checksum on startup.
    pub index_verify: Option<bool>,
}

impl Config {
    pub fn load() -> Config {
        match std::fs::read_to_string(paths::config_file()) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warning: ignoring {}: {e}", paths::config_file().display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn trusted_publishers(&self) -> Vec<String> {
        self.trusted_publishers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect()
    }
}

/// Where the configuration actually came from, for `wing info`.
pub fn config_path() -> PathBuf {
    paths::config_file()
}

/// Returns `true` when the publisher passes the trust list. An empty list
/// allows everything.
pub fn publisher_allowed(trusted: &[String], publisher: Option<&str>) -> bool {
    if trusted.is_empty() {
        return true;
    }
    let Some(p) = publisher else {
        return false;
    };
    trusted.iter().any(|t| t == &p.to_lowercase())
}
