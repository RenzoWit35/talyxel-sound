//! Linking a Spotify account.
//!
//! The browser-based OAuth (PKCE) flow runs once per Spotify app. The resulting refresh
//! tokens are kept in the OS credential store (Windows Credential Manager, macOS Keychain,
//! Secret Service on Linux) and never written to disk in plaintext. Each launch exchanges
//! them for fresh access tokens.
//!
//! Playback always links through Spotify's desktop client, the only one librespot's login
//! accepts. When the config names the user's own developer app, that app is linked as well
//! and its token is used for the Web API, so requests don't count against the desktop
//! client's shared rate limit.

use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use tracing::{info, warn};

use crate::config::{APP_NAME, Config};

/// Spotify's desktop client id, used by librespot-based players.
pub const DESKTOP_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const KEYRING_USER: &str = "spotify-refresh-token";
/// Prefix of the keyring entry for the user's own developer app, followed by its client id.
const OWN_APP_KEYRING_PREFIX: &str = "web-api-refresh-token:";

pub const SCOPES: &[&str] = &[
    "streaming",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-read-private",
    "user-read-email",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "user-top-read",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
];

const LOGIN_DONE_HTML: &str = r#"<!doctype html>
<html><head><title>Talyxel Sound</title></head>
<body style="background:#0d1a12;color:#7ee29a;font-family:monospace;text-align:center;padding-top:15vh">
<h1>Talyxel Sound is linked to Spotify</h1><p>You can close this tab and return to your terminal.</p>
</body></html>"#;

/// A Spotify app the account is linked to, and where its refresh token is kept.
struct AppLink {
    client_id: String,
    redirect_uri: String,
    keyring_user: String,
    own_app: bool,
}

impl AppLink {
    /// Spotify's desktop client, used for playback.
    fn desktop() -> Self {
        Self {
            client_id: DESKTOP_CLIENT_ID.to_string(),
            redirect_uri: DEFAULT_REDIRECT_URI.to_string(),
            keyring_user: KEYRING_USER.to_string(),
            own_app: false,
        }
    }

    /// The user's own developer app, if the config names one.
    fn own_app(cfg: &Config) -> Option<Self> {
        let id = cfg
            .client_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())?;
        Some(Self {
            client_id: id.to_string(),
            redirect_uri: cfg
                .redirect_uri
                .clone()
                .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string()),
            keyring_user: format!("{OWN_APP_KEYRING_PREFIX}{id}"),
            own_app: true,
        })
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(APP_NAME, &self.keyring_user).context("OS credential store unavailable")
    }

    fn save(&self, refresh_token: &str) {
        store(&self.keyring_user, refresh_token);
    }

    fn forget(&self) -> Result<bool> {
        match self.entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn oauth_client(&self) -> Result<OAuthClient> {
        OAuthClientBuilder::new(&self.client_id, &self.redirect_uri, SCOPES.to_vec())
            .open_in_browser()
            .with_custom_message(LOGIN_DONE_HTML)
            .build()
            .map_err(|e| anyhow!("could not build OAuth client: {e}"))
    }

    /// Runs the interactive browser login and stores the refresh token.
    async fn login(&self) -> Result<OAuthToken> {
        if self.own_app {
            println!(
                "Opening your browser to allow your Spotify app ({}) to use your account…",
                self.client_id
            );
        } else {
            println!("Opening your browser to link Talyxel Sound with Spotify…");
        }
        println!("If it does not open, copy the \"Browse to\" URL below into your browser.");
        let token = self
            .oauth_client()?
            .get_access_token_async()
            .await
            .map_err(|e| anyhow!("Spotify login failed: {e}"))?;
        self.save(&token.refresh_token);
        info!("linked Spotify app {} via browser login", self.client_id);
        Ok(token)
    }

    /// Returns a valid access token, using the stored refresh token when possible and
    /// falling back to the browser login.
    async fn obtain(&self) -> Result<OAuthToken> {
        if let Some(refresh) = self.entry().ok().and_then(|e| e.get_password().ok()) {
            match self.oauth_client()?.refresh_token_async(&refresh).await {
                Ok(mut token) => {
                    if token.refresh_token.is_empty() {
                        token.refresh_token = refresh;
                    }
                    self.save(&token.refresh_token);
                    return Ok(token);
                }
                Err(e) => warn!("stored Spotify credentials rejected, logging in again: {e}"),
            }
        }
        self.login().await
    }
}

fn store(keyring_user: &str, refresh_token: &str) {
    let saved =
        keyring::Entry::new(APP_NAME, keyring_user).and_then(|e| e.set_password(refresh_token));
    if let Err(e) = saved {
        warn!("could not store Spotify credentials in the OS keyring: {e:#}");
    }
}

/// Stores a refreshed token for the desktop (playback) client.
pub fn save_refresh_token(token: &str) {
    store(KEYRING_USER, token);
}

/// Stores a refreshed token for the user's own developer app.
pub fn save_own_app_refresh_token(client_id: &str, token: &str) {
    store(&format!("{OWN_APP_KEYRING_PREFIX}{client_id}"), token);
}

/// Removes the stored credentials; returns whether there were any.
pub fn logout(cfg: &Config) -> Result<bool> {
    let mut removed = AppLink::desktop().forget()?;
    if let Some(own) = AppLink::own_app(cfg) {
        removed |= own.forget()?;
    }
    Ok(removed)
}

/// Runs the browser login for playback and, if configured, for the user's own app.
pub async fn login(cfg: &Config) -> Result<()> {
    AppLink::desktop().login().await?;
    if let Some(own) = AppLink::own_app(cfg) {
        own.login().await?;
    }
    Ok(())
}

/// Access token for playback (Spotify's desktop client).
pub async fn obtain_token() -> Result<OAuthToken> {
    AppLink::desktop().obtain().await
}

/// Access token and client id of the user's own developer app, if the config names one.
pub async fn obtain_own_app_token(cfg: &Config) -> Result<Option<(OAuthToken, String)>> {
    let Some(own) = AppLink::own_app(cfg) else {
        return Ok(None);
    };
    let token = own.obtain().await?;
    Ok(Some((token, own.client_id)))
}

/// Converts a librespot OAuth token into an rspotify token.
pub fn to_rspotify_token(token: &OAuthToken) -> rspotify::Token {
    let remaining = token.expires_at.saturating_duration_since(Instant::now());
    let expires_in = chrono::TimeDelta::from_std(remaining).unwrap_or_default();
    rspotify::Token {
        access_token: token.access_token.clone(),
        expires_in,
        expires_at: Some(chrono::Utc::now() + expires_in),
        refresh_token: Some(token.refresh_token.clone()),
        scopes: token.scopes.iter().cloned().collect(),
    }
}
