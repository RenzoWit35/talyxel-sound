//! Linking a Spotify account.
//!
//! The browser-based OAuth (PKCE) flow runs once. The resulting refresh token is kept in the
//! OS credential store (Windows Credential Manager, macOS Keychain, Secret Service on Linux)
//! and never written to disk in plaintext. Each launch exchanges it for a fresh access token,
//! which authenticates both the librespot session and the Web API client.

use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use librespot_oauth::{OAuthClientBuilder, OAuthToken};
use tracing::{info, warn};

use crate::config::{APP_NAME, Config};

/// Client id + redirect URI registered for librespot-based clients.
const DEFAULT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const KEYRING_USER: &str = "spotify-refresh-token";

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

pub fn client_id(cfg: &Config) -> &str {
    match (&cfg.client_id, &cfg.redirect_uri) {
        (Some(id), Some(_)) => id,
        _ => DEFAULT_CLIENT_ID,
    }
}

fn redirect_uri(cfg: &Config) -> &str {
    match (&cfg.client_id, &cfg.redirect_uri) {
        (Some(_), Some(uri)) => uri,
        _ => DEFAULT_REDIRECT_URI,
    }
}

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(APP_NAME, KEYRING_USER).context("OS credential store unavailable")
}

pub fn load_refresh_token() -> Option<String> {
    entry().ok()?.get_password().ok()
}

pub fn save_refresh_token(token: &str) {
    match entry().and_then(|e| e.set_password(token).map_err(Into::into)) {
        Ok(()) => {}
        Err(e) => warn!("could not store Spotify credentials in the OS keyring: {e:#}"),
    }
}

pub fn logout() -> Result<bool> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn oauth_client(cfg: &Config) -> Result<librespot_oauth::OAuthClient> {
    OAuthClientBuilder::new(client_id(cfg), redirect_uri(cfg), SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message(LOGIN_DONE_HTML)
        .build()
        .map_err(|e| anyhow!("could not build OAuth client: {e}"))
}

/// Runs the interactive browser login and stores the refresh token.
pub async fn login(cfg: &Config) -> Result<OAuthToken> {
    println!("Opening your browser to link Talyxel Sound with Spotify…");
    println!("If it does not open, copy the \"Browse to\" URL below into your browser.");
    let token = oauth_client(cfg)?
        .get_access_token_async()
        .await
        .map_err(|e| anyhow!("Spotify login failed: {e}"))?;
    save_refresh_token(&token.refresh_token);
    info!("linked Spotify account via browser login");
    Ok(token)
}

/// Returns a valid access token, using the stored refresh token when possible and
/// falling back to the browser login.
pub async fn obtain_token(cfg: &Config) -> Result<OAuthToken> {
    if let Some(refresh) = load_refresh_token() {
        match oauth_client(cfg)?.refresh_token_async(&refresh).await {
            Ok(mut token) => {
                if token.refresh_token.is_empty() {
                    token.refresh_token = refresh;
                }
                save_refresh_token(&token.refresh_token);
                return Ok(token);
            }
            Err(e) => warn!("stored Spotify credentials rejected, logging in again: {e}"),
        }
    }
    login(cfg).await
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
