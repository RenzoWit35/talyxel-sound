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
    /// Client id of the user's own Spotify developer app. When set, Web API requests use it
    /// instead of the desktop client, whose rate limit is shared with other players.
    /// Playback always uses the desktop client.
    pub client_id: Option<String>,
    /// Redirect URI registered for `client_id` (default: http://127.0.0.1:8898/login).
    pub redirect_uri: Option<String>,
    /// Built-in color theme: green, blue, purple, amber, red or mono (`t` cycles them).
    pub theme: String,
    /// Colors that replace the theme's own.
    #[serde(skip_serializing_if = "ColorOverrides::is_empty")]
    pub colors: ColorOverrides,
}

/// The `[colors]` table: `"#7ee29a"`, a name such as `"cyan"`, or a number from 0 to 255.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ColorOverrides {
    pub text: Option<String>,
    pub accent: Option<String>,
    pub dim: Option<String>,
    pub border: Option<String>,
    pub muted: Option<String>,
    pub error: Option<String>,
    pub badge: Option<String>,
    pub selection_text: Option<String>,
    /// Unset by default, so the terminal's own background shows.
    pub background: Option<String>,
}

impl ColorOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
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
            update_repo_owner: "RenzoWit35".to_string(),
            update_repo_name: "talyxel-sound".to_string(),
            client_id: None,
            redirect_uri: None,
            theme: "green".to_string(),
            colors: ColorOverrides::default(),
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

/// `config.toml` with `theme` set to `theme`, keeping everything else as it was written,
/// comments included.
pub fn with_theme(text: &str, theme: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("invalid config.toml")?;
    doc["theme"] = toml_edit::value(theme);
    Ok(doc.to_string())
}

/// Remembers the chosen theme in the config file at `path`.
pub fn save_theme(path: &std::path::Path, theme: &str) -> Result<()> {
    let text = fs::read_to_string(path).unwrap_or_default();
    fs::write(path, with_theme(&text, theme)?)
        .with_context(|| format!("writing {}", path.display()))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
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
    fn theme_and_colors_are_read() {
        let cfg = Config::parse(
            "theme = \"blue\"\n\n[colors]\naccent = \"#ff0000\"\nbackground = \"black\"\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, "blue");
        assert_eq!(cfg.colors.accent.as_deref(), Some("#ff0000"));
        assert_eq!(cfg.colors.background.as_deref(), Some("black"));
        assert_eq!(cfg.colors.text, None);
        assert_eq!(Config::default().theme, "green");
    }

    #[test]
    fn saving_the_theme_keeps_the_rest_of_the_file() {
        let text = "# my settings\nbitrate = 160 # quality\n\n[colors]\naccent = \"#ff0000\"\n";
        let saved = with_theme(text, "amber").unwrap();
        assert!(saved.contains("# my settings"), "{saved}");
        assert!(saved.contains("bitrate = 160 # quality"), "{saved}");
        assert!(saved.contains("accent = \"#ff0000\""), "{saved}");
        let cfg = Config::parse(&saved).unwrap();
        assert_eq!((cfg.theme.as_str(), cfg.bitrate), ("amber", 160));
        assert_eq!(cfg.colors.accent.as_deref(), Some("#ff0000"));

        let again = with_theme(&saved, "blue").unwrap();
        assert_eq!(again.matches("theme =").count(), 1, "{again}");
        assert_eq!(Config::parse(&again).unwrap().theme, "blue");
    }

    #[test]
    fn saving_the_theme_to_a_file() {
        let path = std::env::temp_dir().join(format!("talyxel-test-{}.toml", uuid::Uuid::new_v4()));
        fs::write(&path, "bitrate = 96\n").unwrap();
        save_theme(&path, "mono").unwrap();
        let cfg = Config::parse(&fs::read_to_string(&path).unwrap()).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!((cfg.theme.as_str(), cfg.bitrate), ("mono", 96));
    }

    #[test]
    fn default_config_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), Config::default());
    }
}
