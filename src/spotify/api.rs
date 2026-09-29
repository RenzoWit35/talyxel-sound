//! Spotify Web API access (metadata, library, search and remote control) via rspotify.
//!
//! Web API rate limits are counted per Spotify app (client id). Playback has to use Spotify's
//! desktop client, whose quota every librespot-based player shares, so users can add their
//! own developer app (`client_id` in the config) and give the Web API a quota of its own.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use rspotify::{
    AuthCodePkceSpotify, ClientError, Config as RsConfig, Credentials, OAuth, TokenCallback,
    http::Query,
    model::{
        AdditionalType, FullTrack, PlayableItem, PlaylistId, RepeatState, SearchResult, SearchType,
    },
    prelude::*,
};
use serde::Deserialize;
use tracing::warn;

use super::{Device, PlaybackState, Playlist, Repeat, Track, internal::Internal};

const PAGE: u32 = 50;
/// Upper bound on tracks loaded for one view, to keep huge playlists responsive.
const MAX_TRACKS: usize = 2000;
/// Development-mode apps may request at most 10 search results at a time.
const OWN_APP_SEARCH_PAGE: u32 = 10;
const OWN_APP_SEARCH_PAGES: u32 = 3;
/// How often a request is retried after Spotify answers 429 Too Many Requests.
const RATE_LIMIT_RETRIES: u32 = 2;
/// Longest wait requested by Spotify that is sat out automatically before retrying.
const MAX_AUTO_WAIT: Duration = Duration::from_secs(60);

/// One Spotify app's authenticated client and its rate-limit state.
#[derive(Clone)]
struct Client {
    spotify: AuthCodePkceSpotify,
    /// When Spotify's requested wait ends; shared by all clones so every task waits.
    blocked_until: Arc<Mutex<Option<Instant>>>,
    /// Whether this is the desktop client whose quota other players share.
    shared: bool,
}

impl Client {
    fn new(
        token: rspotify::Token,
        client_id: &str,
        shared: bool,
        on_refresh: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        let config = RsConfig {
            token_refreshing: true,
            token_callback_fn: Arc::new(Some(TokenCallback(Box::new(
                move |t: rspotify::Token| {
                    if let Some(refresh) = t.refresh_token {
                        on_refresh(refresh);
                    }
                    Ok(())
                },
            )))),
            ..RsConfig::default()
        };
        let spotify = AuthCodePkceSpotify::from_token_with_config(
            token,
            Credentials::new_pkce(client_id),
            OAuth::default(),
            config,
        );
        Self {
            spotify,
            blocked_until: Arc::default(),
            shared,
        }
    }

    fn blocked_for(&self) -> Duration {
        let until = *self.blocked_until.lock().unwrap();
        until.map_or(Duration::ZERO, |t| {
            t.saturating_duration_since(Instant::now())
        })
    }

    fn block_for(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut blocked = self.blocked_until.lock().unwrap();
        if blocked.is_none_or(|t| t < until) {
            *blocked = Some(until);
        }
    }

    /// Sends one request, first sitting out any short wait Spotify asked for and retrying
    /// after a 429 while the requested wait is short. During a longer wait the request is
    /// sent anyway, so it fails fast with Spotify's current wait instead of hanging.
    async fn call<T, F, Fut>(&self, mut request: F) -> std::result::Result<T, ClientError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, ClientError>>,
    {
        let mut retries = 0;
        loop {
            let wait = self.blocked_for();
            if !wait.is_zero() && wait <= MAX_AUTO_WAIT {
                tokio::time::sleep(wait).await;
            }
            match request().await {
                Err(e) => {
                    let Some(wait) = rate_limit_delay(&e) else {
                        return Err(e);
                    };
                    warn!("rate limited by Spotify, waiting {wait:?}");
                    self.block_for(wait);
                    if retries == RATE_LIMIT_RETRIES || wait > MAX_AUTO_WAIT {
                        return Err(e);
                    }
                    retries += 1;
                }
                ok => return ok,
            }
        }
    }

    /// Like [`Client::call`], with a readable message for rate-limit errors.
    async fn run<T, F, Fut>(&self, request: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, ClientError>>,
    {
        self.call(request).await.map_err(|e| self.error(e))
    }

    fn error(&self, e: ClientError) -> anyhow::Error {
        match rate_limit_delay(&e) {
            Some(wait) => anyhow!(rate_limit_message(wait, self.shared)),
            None => e.into(),
        }
    }
}

/// Status-bar text for a request Spotify refused with 429 Too Many Requests.
fn rate_limit_message(wait: Duration, shared: bool) -> String {
    let mut msg = format!(
        "Spotify rate limit: try again in {}s",
        wait.as_secs().max(1)
    );
    if shared {
        msg.push_str(" (use your own client_id to avoid this, see README)");
    }
    msg
}

#[derive(Clone)]
pub struct Api {
    /// The user's own developer app when configured, otherwise the desktop client.
    web: Client,
    /// Spotify's desktop client while `web` is the user's own app, for playlists that
    /// development-mode apps are not allowed to read.
    desktop: Option<Client>,
    /// Spotify's own endpoints through the playback session, once it is connected. They
    /// replace the Web API, which refuses the desktop client (see [`Internal`]).
    internal: Option<Internal>,
}

impl Api {
    /// Builds a client that refreshes its own token and reports new refresh tokens
    /// through `on_refresh` so they can be persisted.
    pub fn new(
        token: rspotify::Token,
        client_id: &str,
        on_refresh: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            web: Client::new(token, client_id, true, on_refresh),
            desktop: None,
            internal: None,
        }
    }

    /// Sends Web API requests through the user's own developer app instead, keeping the
    /// current client for playlists the developer app can't read.
    pub fn with_own_app(
        self,
        token: rspotify::Token,
        client_id: &str,
        on_refresh: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            web: Client::new(token, client_id, false, on_refresh),
            desktop: Some(self.web),
            internal: self.internal,
        }
    }

    /// Serves the library, search and Spotify Connect through the librespot session
    /// instead of the Web API.
    pub fn with_internal(self, internal: Internal) -> Self {
        Self {
            internal: Some(internal),
            ..self
        }
    }

    pub fn internal(&self) -> Option<&Internal> {
        self.internal.as_ref()
    }

    /// How long Spotify asked the Web API client to wait before its next request.
    pub fn rate_limited_for(&self) -> Duration {
        self.web.blocked_for()
    }

    pub async fn username(&self) -> Result<String> {
        if let Some(i) = &self.internal {
            return i.display_name().await;
        }
        let c = &self.web;
        let me = c.run(|| c.spotify.me()).await?;
        Ok(me.display_name.unwrap_or_else(|| me.id.id().to_string()))
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        if let Some(i) = &self.internal {
            return i.playlists().await;
        }
        let c = &self.web;
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let offset_param = offset.to_string();
            let limit_param = PAGE.to_string();
            let query = Query::from([("limit", limit_param.as_str()), ("offset", &offset_param)]);
            let body = c.run(|| c.spotify.api_get("me/playlists", &query)).await?;
            let (playlists, n, more) = parse_playlist_page(&body)?;
            out.extend(playlists);
            offset += n;
            if !more || n == 0 {
                break;
            }
        }
        Ok(out)
    }

    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        if let Some(i) = &self.internal {
            return i.playlist_tracks(playlist_id).await;
        }
        let id = PlaylistId::from_id(playlist_id)?;
        let result = fetch_playlist_tracks(&self.web, &id).await;
        let usable = result.as_ref().is_ok_and(|tracks| !tracks.is_empty());
        match &self.desktop {
            // Development-mode apps only get the items of playlists the user owns or
            // collaborates on, so load everything else through the desktop client.
            Some(desktop) if !usable => fetch_playlist_tracks(desktop, &id).await,
            _ => result,
        }
    }

    pub async fn liked_tracks(&self) -> Result<Vec<Track>> {
        if let Some(i) = &self.internal {
            return i.liked_tracks().await;
        }
        let c = &self.web;
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page = c
                .run(|| {
                    c.spotify
                        .current_user_saved_tracks_manual(None, Some(PAGE), Some(offset))
                })
                .await?;
            let n = page.items.len() as u32;
            out.extend(page.items.into_iter().filter_map(|s| full_track(s.track)));
            offset += n;
            if page.next.is_none() || n == 0 || out.len() >= MAX_TRACKS {
                break;
            }
        }
        Ok(out)
    }

    pub async fn liked_total(&self) -> Result<(u32, Option<String>)> {
        if let Some(i) = &self.internal {
            return i.liked_head().await;
        }
        let c = &self.web;
        let page = c
            .run(|| {
                c.spotify
                    .current_user_saved_tracks_manual(None, Some(1), Some(0))
            })
            .await?;
        let first = page
            .items
            .into_iter()
            .next()
            .and_then(|s| full_track(s.track))
            .map(|t| t.uri);
        Ok((page.total, first))
    }

    pub async fn recently_played(&self) -> Result<Vec<Track>> {
        if let Some(i) = &self.internal {
            return i.recently_played().await;
        }
        let c = &self.web;
        let page = c
            .run(|| c.spotify.current_user_recently_played(Some(50), None))
            .await?;
        Ok(page
            .items
            .into_iter()
            .filter_map(|h| full_track(h.track))
            .collect())
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Track>> {
        if let Some(i) = &self.internal {
            return i.search(query).await;
        }
        let c = &self.web;
        let (limit, pages) = if self.desktop.is_some() {
            (OWN_APP_SEARCH_PAGE, OWN_APP_SEARCH_PAGES)
        } else {
            (PAGE, 1)
        };
        let mut out = Vec::new();
        for page in 0..pages {
            let offset = page * limit;
            let result = c
                .run(|| {
                    c.spotify.search(
                        query,
                        SearchType::Track,
                        None,
                        None,
                        Some(limit),
                        Some(offset),
                    )
                })
                .await?;
            let SearchResult::Tracks(page) = result else {
                break;
            };
            let more = page.next.is_some();
            out.extend(page.items.into_iter().filter_map(full_track));
            if !more {
                break;
            }
        }
        Ok(out)
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        if let Some(i) = &self.internal {
            return i.devices().await;
        }
        let c = &self.web;
        Ok(c.run(|| c.spotify.device())
            .await?
            .into_iter()
            .map(convert_device)
            .collect())
    }

    pub async fn playback(&self) -> Result<Option<PlaybackState>> {
        if let Some(i) = &self.internal {
            return i.playback().await;
        }
        let c = &self.web;
        let ctx = c
            .run(|| {
                c.spotify.current_playback(
                    None,
                    Some(&[AdditionalType::Track, AdditionalType::Episode]),
                )
            })
            .await?;
        Ok(ctx.map(|c| PlaybackState {
            device: Some(convert_device(c.device)),
            track: c.item.and_then(playable_to_track),
            is_playing: c.is_playing,
            progress_ms: c.progress.map_or(0, |p| p.num_milliseconds().max(0) as u32),
            shuffle: c.shuffle_state,
            repeat: match c.repeat_state {
                RepeatState::Off => Repeat::Off,
                RepeatState::Context => Repeat::Context,
                RepeatState::Track => Repeat::Track,
            },
            context_uri: c.context.map(|c| c.uri),
        }))
    }

    // ---- Remote control (used when another device is playing) ----

    /// Moves playback to `device_id` and keeps it playing from `position_ms`.
    pub async fn transfer(&self, device_id: &str, position_ms: Option<u32>) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.transfer(device_id, position_ms).await;
        }
        let c = &self.web;
        c.run(|| c.spotify.transfer_playback(device_id, Some(true)))
            .await
    }
    pub async fn pause(&self) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.pause().await;
        }
        let c = &self.web;
        c.run(|| c.spotify.pause_playback(None)).await
    }
    pub async fn resume(&self) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.resume().await;
        }
        let c = &self.web;
        c.run(|| c.spotify.resume_playback(None, None)).await
    }
    pub async fn next(&self) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.next().await;
        }
        let c = &self.web;
        c.run(|| c.spotify.next_track(None)).await
    }
    pub async fn prev(&self) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.prev().await;
        }
        let c = &self.web;
        c.run(|| c.spotify.previous_track(None)).await
    }
    pub async fn seek(&self, position_ms: u32) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.seek(position_ms).await;
        }
        let c = &self.web;
        let pos = chrono::TimeDelta::milliseconds(position_ms as i64);
        c.run(|| c.spotify.seek_track(pos, None)).await
    }
    pub async fn volume(&self, percent: u8) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.volume(percent).await;
        }
        let c = &self.web;
        c.run(|| c.spotify.volume(percent.min(100), None)).await
    }
    pub async fn shuffle(&self, on: bool) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.shuffle(on).await;
        }
        let c = &self.web;
        c.run(|| c.spotify.shuffle(on, None)).await
    }
    pub async fn repeat(&self, repeat: Repeat) -> Result<()> {
        if let Some(i) = &self.internal {
            return i.repeat(repeat).await;
        }
        let c = &self.web;
        let state = match repeat {
            Repeat::Off => RepeatState::Off,
            Repeat::Context => RepeatState::Context,
            Repeat::Track => RepeatState::Track,
        };
        c.run(|| c.spotify.repeat(state, None)).await
    }
}

async fn fetch_playlist_tracks(c: &Client, id: &PlaylistId<'_>) -> Result<Vec<Track>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let page = c
            .run(|| {
                c.spotify
                    .playlist_items_manual(id.clone(), None, None, Some(100), Some(offset))
            })
            .await?;
        let n = page.items.len() as u32;
        out.extend(
            page.items
                .into_iter()
                .filter_map(|i| i.item.and_then(playable_to_track)),
        );
        offset += n;
        if page.next.is_none() || n == 0 || out.len() >= MAX_TRACKS {
            break;
        }
    }
    Ok(out)
}

/// A page of `GET /me/playlists`, parsed by hand: development-mode apps don't get the
/// `items` summary for playlists the user doesn't own, which rspotify's model can't handle.
#[derive(Deserialize)]
struct PlaylistPage {
    #[serde(default)]
    items: Vec<Option<RawPlaylist>>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct RawPlaylist {
    id: String,
    uri: Option<String>,
    name: String,
    #[serde(default)]
    snapshot_id: String,
    items: Option<ItemsRef>,
    /// Name of `items` before Spotify's February 2026 API changes.
    tracks: Option<ItemsRef>,
}

#[derive(Deserialize)]
struct ItemsRef {
    #[serde(default)]
    total: u32,
}

/// Returns the playlists on one page, how many entries the page had, and whether
/// there are more pages.
fn parse_playlist_page(body: &str) -> Result<(Vec<Playlist>, u32, bool)> {
    let page: PlaylistPage = serde_json::from_str(body)?;
    let n = page.items.len() as u32;
    let playlists = page
        .items
        .into_iter()
        .flatten()
        .map(|p| Playlist {
            uri: p
                .uri
                .unwrap_or_else(|| format!("spotify:playlist:{}", p.id)),
            total: p.items.or(p.tracks).map_or(0, |r| r.total),
            id: p.id,
            name: p.name,
            snapshot_id: p.snapshot_id,
        })
        .collect();
    Ok((playlists, n, page.next.is_some()))
}

/// If the error is an HTTP 429, returns how long Spotify asked us to wait.
pub fn rate_limit_delay(err: &ClientError) -> Option<Duration> {
    if let ClientError::Http(http) = err
        && let rspotify::http::HttpError::StatusCode(resp) = http.as_ref()
        && resp.status().as_u16() == 429
    {
        let secs = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5);
        return Some(Duration::from_secs(secs.max(1)));
    }
    None
}

fn convert_device(d: rspotify::model::Device) -> Device {
    Device {
        id: d.id,
        name: d.name,
        is_active: d.is_active,
        volume: d.volume_percent.map(|v| v.min(100) as u8),
        kind: String::new(),
    }
}

fn full_track(t: FullTrack) -> Option<Track> {
    let uri = t.id.as_ref()?.uri();
    Some(Track {
        uri,
        name: t.name,
        artists: t
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        album: t.album.name,
        year: t.album.release_date.map(|d| d.chars().take(4).collect()),
        duration_ms: t.duration.num_milliseconds().max(0) as u32,
    })
}

fn playable_to_track(item: PlayableItem) -> Option<Track> {
    match item {
        PlayableItem::Track(t) => full_track(t),
        PlayableItem::Episode(e) => Some(Track {
            uri: e.id.uri(),
            name: e.name,
            artists: e.show.name.clone(),
            album: e.show.name,
            year: Some(e.release_date.chars().take(4).collect()),
            duration_ms: e.duration.num_milliseconds().max(0) as u32,
        }),
        PlayableItem::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering::SeqCst};

    use super::*;

    fn too_many_requests(retry_after: &str) -> ClientError {
        let response = http::Response::builder()
            .status(429)
            .header("retry-after", retry_after)
            .body("")
            .unwrap();
        ClientError::Http(Box::new(rspotify::http::HttpError::StatusCode(
            reqwest::Response::from(response),
        )))
    }

    fn client() -> Client {
        Client::new(rspotify::Token::default(), "test", true, |_| {})
    }

    #[tokio::test]
    async fn waits_out_a_short_rate_limit_and_retries() {
        let c = client();
        let calls = AtomicU32::new(0);
        let started = Instant::now();
        let result = c
            .call(|| async {
                match calls.fetch_add(1, SeqCst) {
                    0 => Err(too_many_requests("1")),
                    _ => Ok(7),
                }
            })
            .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.load(SeqCst), 2);
        assert!(started.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn gives_up_after_the_retry_limit() {
        let c = client();
        let calls = AtomicU32::new(0);
        let result: std::result::Result<(), _> = c
            .call(|| async {
                calls.fetch_add(1, SeqCst);
                Err(too_many_requests("1"))
            })
            .await;
        assert!(rate_limit_delay(&result.unwrap_err()).is_some());
        assert_eq!(calls.load(SeqCst), RATE_LIMIT_RETRIES + 1);
    }

    #[tokio::test]
    async fn long_rate_limit_fails_fast_and_holds_back_other_requests() {
        let c = client();
        let started = Instant::now();
        let result: std::result::Result<(), _> =
            c.call(|| async { Err(too_many_requests("3600")) }).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        let err = c.error(result.unwrap_err()).to_string();
        assert!(
            err.starts_with("Spotify rate limit: try again in 3600s"),
            "{err}"
        );
        assert!(err.contains("client_id"), "{err}");
        // Clones (other tasks) see the same wait.
        assert!(c.clone().blocked_for() > Duration::from_secs(3590));
    }

    #[tokio::test]
    async fn does_not_hang_while_a_long_wait_is_pending() {
        let c = client();
        c.block_for(Duration::from_secs(3600));
        let started = Instant::now();
        let result = c.call(|| async { Ok(1) }).await;
        assert_eq!(result.unwrap(), 1);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_shorter_wait_never_shortens_an_existing_block() {
        let c = client();
        c.block_for(Duration::from_secs(60));
        c.block_for(Duration::from_secs(1));
        assert!(c.blocked_for() > Duration::from_secs(50));
    }

    #[test]
    fn parses_playlists_with_and_without_item_counts() {
        let body = r#"{
            "items": [
                {"id": "a", "uri": "spotify:playlist:a", "name": "Mine", "snapshot_id": "s1",
                 "items": {"href": "x", "total": 12}},
                {"id": "b", "name": "Old format", "snapshot_id": "s2",
                 "tracks": {"href": "x", "total": 3}},
                {"id": "c", "name": "Followed", "snapshot_id": "s3"},
                null
            ],
            "next": "https://api.spotify.com/v1/me/playlists?offset=4"
        }"#;
        let (playlists, n, more) = parse_playlist_page(body).unwrap();
        assert_eq!(n, 4);
        assert!(more);
        let summary: Vec<_> = playlists
            .iter()
            .map(|p| (p.id.as_str(), p.uri.as_str(), p.total))
            .collect();
        assert_eq!(
            summary,
            [
                ("a", "spotify:playlist:a", 12),
                ("b", "spotify:playlist:b", 3),
                ("c", "spotify:playlist:c", 0),
            ]
        );
    }
}
