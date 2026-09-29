//! Spotify Web API access (metadata, library, search and remote control) via rspotify.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use rspotify::{
    AuthCodePkceSpotify, ClientError, Config as RsConfig, Credentials, OAuth, TokenCallback,
    model::{
        AdditionalType, FullTrack, PlayableItem, PlaylistId, RepeatState, SearchResult, SearchType,
    },
    prelude::*,
};

use super::{Device, PlaybackState, Playlist, Repeat, Track};

const PAGE: u32 = 50;
/// Upper bound on tracks loaded for one view, to keep huge playlists responsive.
const MAX_TRACKS: usize = 2000;

#[derive(Clone)]
pub struct Api {
    client: AuthCodePkceSpotify,
}

impl Api {
    /// Builds a client that refreshes its own token and reports new refresh tokens
    /// through `on_refresh` so they can be persisted.
    pub fn new(
        token: rspotify::Token,
        client_id: &str,
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
        let client = AuthCodePkceSpotify::from_token_with_config(
            token,
            Credentials::new_pkce(client_id),
            OAuth::default(),
            config,
        );
        Self { client }
    }

    pub async fn username(&self) -> Result<String> {
        let me = self.client.me().await?;
        Ok(me.display_name.unwrap_or_else(|| me.id.id().to_string()))
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .current_user_playlists_manual(Some(PAGE), Some(offset))
                .await?;
            let n = page.items.len() as u32;
            out.extend(page.items.into_iter().map(|p| Playlist {
                id: p.id.id().to_string(),
                uri: p.id.uri(),
                name: p.name,
                snapshot_id: p.snapshot_id,
                total: p.items.total,
            }));
            offset += n;
            if page.next.is_none() || n == 0 {
                break;
            }
        }
        Ok(out)
    }

    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let id = PlaylistId::from_id(playlist_id)?;
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .playlist_items_manual(id.clone(), None, None, Some(100), Some(offset))
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

    pub async fn liked_tracks(&self) -> Result<Vec<Track>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .current_user_saved_tracks_manual(None, Some(PAGE), Some(offset))
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
        let page = self
            .client
            .current_user_saved_tracks_manual(None, Some(1), Some(0))
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
        let page = self
            .client
            .current_user_recently_played(Some(50), None)
            .await?;
        Ok(page
            .items
            .into_iter()
            .filter_map(|h| full_track(h.track))
            .collect())
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Track>> {
        let result = self
            .client
            .search(query, SearchType::Track, None, None, Some(PAGE), None)
            .await?;
        Ok(match result {
            SearchResult::Tracks(page) => page.items.into_iter().filter_map(full_track).collect(),
            _ => Vec::new(),
        })
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        Ok(self
            .client
            .device()
            .await?
            .into_iter()
            .map(convert_device)
            .collect())
    }

    pub async fn playback(&self) -> std::result::Result<Option<PlaybackState>, ClientError> {
        let ctx = self
            .client
            .current_playback(
                None,
                Some(&[AdditionalType::Track, AdditionalType::Episode]),
            )
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

    pub async fn transfer(&self, device_id: &str, play: bool) -> Result<()> {
        Ok(self.client.transfer_playback(device_id, Some(play)).await?)
    }
    pub async fn pause(&self) -> Result<()> {
        Ok(self.client.pause_playback(None).await?)
    }
    pub async fn resume(&self) -> Result<()> {
        Ok(self.client.resume_playback(None, None).await?)
    }
    pub async fn next(&self) -> Result<()> {
        Ok(self.client.next_track(None).await?)
    }
    pub async fn prev(&self) -> Result<()> {
        Ok(self.client.previous_track(None).await?)
    }
    pub async fn seek(&self, position_ms: u32) -> Result<()> {
        let pos = chrono::TimeDelta::milliseconds(position_ms as i64);
        Ok(self.client.seek_track(pos, None).await?)
    }
    pub async fn volume(&self, percent: u8) -> Result<()> {
        Ok(self.client.volume(percent.min(100), None).await?)
    }
    pub async fn shuffle(&self, on: bool) -> Result<()> {
        Ok(self.client.shuffle(on, None).await?)
    }
    pub async fn repeat(&self, repeat: Repeat) -> Result<()> {
        let state = match repeat {
            Repeat::Off => RepeatState::Off,
            Repeat::Context => RepeatState::Context,
            Repeat::Track => RepeatState::Track,
        };
        Ok(self.client.repeat(state, None).await?)
    }
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
