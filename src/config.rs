use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "talyxel-sound";
pub const APP_TITLE: &str = "Talyxel Sound";

/// User configuration, stored as TOML in the platform config directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Name shown in the Spotify Connect device list.
    pub device_name: String,
    /// Streaming bitrate: 96, 160 or 320.
    pub bitrate: u16,
    /// Initial volume in percent (0-100).
    pub initial_volume: u8,
    /// Poll interval for remote playback state while music is playing elsewhere (ms).
    pub remote_poll_active_ms: u64,
    /// Poll interval for remote playback state while idle (ms).
    pub remote_poll_idle_ms: u64,
    /// How often playlists are checked for changes (seconds).
    pub library_refresh_secs: u64,
    /// Check GitHub Releases for a newer version on startup.
    pub check_updates: bool,
    pub update_repo_owner: String,
    pub update_repo_name: String,
    /// Optional own Spotify developer app. Both must be set to take effect.
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: APP_TITLE.to_string(),
            bitrate: 320,
            initial_volume: 75,
            remote_poll_active_ms: 1000,
            remote_poll_idle_ms: 5000,
            library_refresh_secs: 60,
            check_updates: true,
            update_repo_owner: "your-github-user".to_string(),
            update_repo_name: "talyxel-sound".to_string(),
            client_id: None,
            redirect_uri: None,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("invalid config.toml")
    }

    /// Loads the config, writing the defaults on first run.
    pub fn load_or_create() -> Result<Self> {
        let path = config_dir()?.join("config.toml");
        if path.exists() {
            let text =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            Self::parse(&text)
        } else {
            let cfg = Self::default();
            fs::write(&path, toml::to_string_pretty(&cfg)?)
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(cfg)
        }
    }
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("no config directory on this platform")?
        .join(APP_NAME);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .context("no data directory on this platform")?
        .join(APP_NAME);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A stable Connect device id, so Spotify sees the same device on every launch.
pub fn device_id() -> Result<String> {
    let path = data_dir()?.join("device_id");
    if let Ok(id) = fs::read_to_string(&path) {
        let id = id.trim();
        if !id.is_empty() {
            return Ok(id.to_string());
        }
    }
    let id = uuid::Uuid::new_v4().as_hyphenated().to_string();
    fs::write(&path, &id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_config_overrides_fields() {
        let cfg = Config::parse("bitrate = 160\ncheck_updates = false\n").unwrap();
        assert_eq!(cfg.bitrate, 160);
        assert!(!cfg.check_updates);
        assert_eq!(cfg.device_name, APP_TITLE);
    }

    #[test]
    fn default_config_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), Config::default());
    }
}
