//! Spotify's own client endpoints (spclient), reached through the librespot session.
//!
//! Since December 2025 Spotify answers Web API requests made with the desktop client's
//! token with 429 Too Many Requests, often asking for a 24 hour wait. Playback has to log in
//! as that client, so without a `client_id` of their own users lost search, playlists, Liked
//! Songs and devices. The official apps don't use the Web API for these: they resolve
//! contexts, read the playlist rootlist, batch metadata requests and follow the Connect
//! cluster. This module does the same, so the library works with nothing but a Premium login.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use futures::{StreamExt, TryStreamExt};
use http::{HeaderMap, HeaderValue, Method, header::CONTENT_TYPE};
use librespot_core::{Session, dealer::protocol::TransferOptions, spclient::TransferRequest};
use librespot_protocol as proto;
use proto::{
    connect::{
        Capabilities, Cluster, ClusterUpdate, Device as ConnectDevice, DeviceInfo, MemberType,
        PutStateReason, PutStateRequest,
    },
    context_page::ContextPage,
    devices::DeviceType,
    extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery},
    extension_kind::ExtensionKind,
    playlist4_external::SelectedListContent,
};
use protobuf::{EnumOrUnknown, Message, MessageField};
use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use super::{
    Device, PlaybackState, Playlist, Repeat, Track,
    player::{percent_to_volume, volume_to_percent},
};

/// librespot's HTTP client sits out any 429 wait by itself, which can be a day, so every
/// request gets a deadline of its own.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound on tracks loaded for one view, to keep huge libraries responsive.
const MAX_TRACKS: usize = 5000;
/// Items per extended-metadata request, and how many of those run at once.
const METADATA_BATCH: usize = 250;
const METADATA_PARALLEL: usize = 4;
const ROOTLIST_PAGE: usize = 200;
/// The metadata cache is dropped when it grows past this many items.
const CACHE_LIMIT: usize = 20_000;
/// How often, and how many times, a transfer is checked until the other device has taken over.
const TRANSFER_CHECK_INTERVAL: Duration = Duration::from_millis(500);
const TRANSFER_CHECKS: usize = 10;
/// How far the position may be off after a transfer before it is corrected.
const POSITION_SLACK_MS: u32 = 3000;

/// Library, search and Connect access through the librespot session.
#[derive(Clone)]
pub struct Internal {
    session: Session,
    /// Separate connection for Spotify Connect requests (see [`super::observer`]).
    observer: Option<Session>,
    /// Metadata of tracks shown so far, so views and now-playing lookups don't repeat it.
    cache: Arc<Mutex<HashMap<String, Track>>>,
}

impl Internal {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            observer: None,
            cache: Arc::default(),
        }
    }

    /// Sends Spotify Connect requests through the observer's own connection.
    pub fn with_observer(self, observer: Session) -> Self {
        Self {
            observer: Some(observer),
            ..self
        }
    }

    fn connect(&self) -> Result<&Session> {
        self.observer
            .as_ref()
            .context("Spotify Connect is not available")
    }

    /// Connect device id of this app.
    pub fn device_id(&self) -> &str {
        self.session.device_id()
    }

    pub async fn display_name(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Profile {
            name: Option<String>,
        }
        let user = self.session.username();
        let body = timed(
            "profile",
            self.session
                .spclient()
                .get_user_profile(&user, Some(0), Some(0)),
        )
        .await?;
        let name = serde_json::from_slice::<Profile>(&body)
            .ok()
            .and_then(|p| p.name)
            .filter(|n| !n.is_empty());
        Ok(name.unwrap_or(user))
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut out = Vec::new();
        let mut from = 0;
        loop {
            let bytes = timed(
                "playlists",
                self.session
                    .spclient()
                    .get_rootlist(from, Some(ROOTLIST_PAGE)),
            )
            .await?;
            let list = SelectedListContent::parse_from_bytes(&bytes)?;
            let n = list.contents.items.len();
            out.extend(parse_rootlist(&list));
            from += n;
            if n == 0 || from >= list.length.unwrap_or(0).max(0) as usize {
                break;
            }
        }
        Ok(out)
    }

    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        self.context_tracks(&format!("spotify:playlist:{playlist_id}"))
            .await
    }

    pub fn liked_uri(&self) -> String {
        format!("spotify:user:{}:collection", self.session.username())
    }

    pub async fn liked_tracks(&self) -> Result<Vec<Track>> {
        self.context_tracks(&self.liked_uri()).await
    }

    /// Number of Liked Songs and the newest one, to notice changes cheaply.
    pub async fn liked_head(&self) -> Result<(u32, Option<String>)> {
        let uris = self.context_uris(&self.liked_uri()).await?;
        Ok((uris.len() as u32, uris.into_iter().next()))
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Track>> {
        self.context_tracks(&search_uri(query)).await
    }

    pub async fn recently_played(&self) -> Result<Vec<Track>> {
        let user = self.session.username();
        let endpoint = format!(
            "/recently-played/v3/user/{user}/recently-played?format=json&offset=0&limit=50&filter=default,collection-new-episodes"
        );
        let body = timed(
            "recently played",
            self.session
                .spclient()
                .request_as_json(&Method::GET, &endpoint, None, None),
        )
        .await?;
        self.tracks(&recent_track_uris(&body)?).await
    }

    async fn context_tracks(&self, uri: &str) -> Result<Vec<Track>> {
        let uris = self.context_uris(uri).await?;
        self.tracks(&uris).await
    }

    async fn context_uris(&self, uri: &str) -> Result<Vec<String>> {
        let sp = self.session.spclient();
        let context = timed("context", sp.get_context(uri)).await?;
        let mut pages = context.pages;
        let mut next = next_page(&pages);
        while let Some(url) = next {
            if playable_uris(&pages).len() >= MAX_TRACKS {
                break;
            }
            let body = timed("context page", sp.get_next_page(&url)).await?;
            let options = protobuf_json_mapping::ParseOptions {
                ignore_unknown_fields: true,
                ..Default::default()
            };
            let page: ContextPage = protobuf_json_mapping::parse_from_str_with_options(
                std::str::from_utf8(&body)?,
                &options,
            )
            .context("reading a context page")?;
            pages.push(page);
            next = next_page(&pages);
        }
        let mut uris = playable_uris(&pages);
        uris.truncate(MAX_TRACKS);
        Ok(uris)
    }

    /// Metadata for `uris`, in the same order; items Spotify doesn't know are left out.
    pub async fn tracks(&self, uris: &[String]) -> Result<Vec<Track>> {
        let missing: Vec<String> = {
            let cache = self.cache.lock().unwrap();
            let mut missing: Vec<String> = uris
                .iter()
                .filter(|u| !cache.contains_key(*u))
                .cloned()
                .collect();
            missing.sort();
            missing.dedup();
            missing
        };
        let batches: Vec<Vec<String>> = missing
            .chunks(METADATA_BATCH)
            .map(<[String]>::to_vec)
            .collect();
        let fetched: Vec<Vec<Track>> = futures::stream::iter(batches)
            .map(|batch| {
                let this = self.clone();
                async move { this.fetch_metadata(&batch).await }
            })
            .buffered(METADATA_PARALLEL)
            .try_collect()
            .await?;
        let mut cache = self.cache.lock().unwrap();
        if cache.len() > CACHE_LIMIT {
            cache.clear();
        }
        for t in fetched.into_iter().flatten() {
            cache.insert(t.uri.clone(), t);
        }
        Ok(uris.iter().filter_map(|u| cache.get(u).cloned()).collect())
    }

    async fn fetch_metadata(&self, uris: &[String]) -> Result<Vec<Track>> {
        let request = BatchedEntityRequest {
            entity_request: uris
                .iter()
                .map(|uri| EntityRequest {
                    entity_uri: uri.clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(
                            if uri.starts_with("spotify:episode:") {
                                ExtensionKind::EPISODE_V4
                            } else {
                                ExtensionKind::TRACK_V4
                            },
                        ),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let response = timed(
            "track details",
            self.session.spclient().get_extended_metadata(request),
        )
        .await?;
        let mut out = Vec::new();
        for array in &response.extended_metadata {
            let kind = array.extension_kind.enum_value();
            for item in &array.extension_data {
                let Some(data) = item.extension_data.as_ref() else {
                    continue;
                };
                let track = match kind {
                    Ok(ExtensionKind::TRACK_V4) => {
                        let t = proto::metadata::Track::parse_from_bytes(&data.value)?;
                        track_from_metadata(&item.entity_uri, &t)
                    }
                    Ok(ExtensionKind::EPISODE_V4) => {
                        let e = proto::metadata::Episode::parse_from_bytes(&data.value)?;
                        episode_from_metadata(&item.entity_uri, &e)
                    }
                    _ => continue,
                };
                out.push(track);
            }
        }
        Ok(out)
    }

    // ---- Spotify Connect ----

    /// The account's Connect cluster (devices and playback), read by registering the observer
    /// as a hidden device that can't play, the way Spotify's web player does it.
    async fn cluster(&self, reason: PutStateReason) -> Result<Cluster> {
        debug!("reading the Connect cluster ({reason:?})");
        let observer = self.connect()?;
        let connection_id = observer.connection_id();
        if connection_id.is_empty() {
            bail!("still connecting to Spotify Connect");
        }
        let request = PutStateRequest {
            member_type: EnumOrUnknown::new(MemberType::CONNECT_STATE),
            put_state_reason: EnumOrUnknown::new(reason),
            device: MessageField::some(ConnectDevice {
                device_info: MessageField::some(DeviceInfo {
                    capabilities: MessageField::some(Capabilities {
                        can_be_player: false,
                        hidden: true,
                        needs_full_player_state: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-Spotify-Connection-Id", connection_id.parse()?);
        let endpoint = format!(
            "/connect-state/v1/devices/hobs_{}",
            observer.device_id().replace('-', "")
        );
        let body = timed(
            "Connect devices",
            observer.spclient().request_with_protobuf(
                &Method::PUT,
                &endpoint,
                Some(headers),
                &request,
            ),
        )
        .await?;
        Ok(Cluster::parse_from_bytes(&body)?)
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        Ok(devices_from_cluster(
            &self.cluster(PutStateReason::PICKER_OPENED).await?,
        ))
    }

    pub async fn playback(&self) -> Result<Option<PlaybackState>> {
        let cluster = self.cluster(PutStateReason::NEW_DEVICE).await?;
        Ok(self
            .with_track_details(playback_from_cluster(&cluster))
            .await)
    }

    /// Playback from a cluster update that Spotify pushed to this device.
    pub async fn playback_from_update(&self, update: &ClusterUpdate) -> Option<PlaybackState> {
        self.with_track_details(playback_from_cluster(&update.cluster))
            .await
    }

    async fn with_track_details(&self, state: Option<PlaybackState>) -> Option<PlaybackState> {
        let mut state = state?;
        if let Some(track) = &mut state.track
            && let Ok(found) = self.tracks(std::slice::from_ref(&track.uri)).await
            && let Some(full) = found.into_iter().next()
        {
            let duration_ms = track.duration_ms;
            *track = full;
            if duration_ms > 0 {
                track.duration_ms = duration_ms;
            }
        }
        Some(state)
    }

    async fn active_device(&self) -> Result<String> {
        let cluster = self.cluster(PutStateReason::NEW_DEVICE).await?;
        if cluster.active_device_id.is_empty() {
            bail!("Nothing is playing on any device");
        }
        Ok(cluster.active_device_id)
    }

    /// Sends a player command to the device that is playing.
    async fn command(&self, endpoint: &str, fields: serde_json::Value) -> Result<()> {
        let to = self.active_device().await?;
        self.command_to(&to, endpoint, fields).await
    }

    async fn command_to(&self, to: &str, endpoint: &str, fields: serde_json::Value) -> Result<()> {
        let path = format!(
            "/connect-state/v1/player/command/from/{}/to/{to}",
            self.device_id()
        );
        self.send_json(Method::POST, &path, command_body(endpoint, fields))
            .await
    }

    async fn send_json(&self, method: Method, path: &str, body: serde_json::Value) -> Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let body = body.to_string();
        timed(
            "Connect command",
            self.connect()?
                .spclient()
                .request(&method, path, Some(headers), Some(body.as_bytes())),
        )
        .await?;
        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        self.command("pause", json!({})).await
    }
    pub async fn resume(&self) -> Result<()> {
        self.command("resume", json!({})).await
    }
    pub async fn next(&self) -> Result<()> {
        self.command("skip_next", json!({})).await
    }
    pub async fn prev(&self) -> Result<()> {
        self.command("skip_prev", json!({})).await
    }
    pub async fn seek(&self, position_ms: u32) -> Result<()> {
        // The target position goes in `value`, as Spotify's own clients send it.
        self.command("seek_to", json!({"value": position_ms, "position": 0}))
            .await
    }
    pub async fn shuffle(&self, on: bool) -> Result<()> {
        self.command("set_shuffling_context", json!({"value": on}))
            .await
    }
    pub async fn repeat(&self, repeat: Repeat) -> Result<()> {
        self.command(
            "set_options",
            json!({
                "repeating_context": repeat != Repeat::Off,
                "repeating_track": repeat == Repeat::Track,
            }),
        )
        .await
    }
    pub async fn volume(&self, percent: u8) -> Result<()> {
        let to = self.active_device().await?;
        let path = format!(
            "/connect-state/v1/connect/volume/from/{}/to/{to}",
            self.device_id()
        );
        self.send_json(
            Method::PUT,
            &path,
            json!({"volume": percent_to_volume(percent)}),
        )
        .await
    }

    /// Moves playback to `device_id` from whichever device is playing, and keeps it playing
    /// from `position_ms`, where it was before the move.
    pub async fn transfer(&self, device_id: &str, position_ms: Option<u32>) -> Result<()> {
        let started = std::time::Instant::now();
        let cluster = self.cluster(PutStateReason::NEW_DEVICE).await?;
        let from = match cluster.active_device_id.as_str() {
            "" => device_id,
            active => active,
        };
        let request = TransferRequest {
            transfer_options: TransferOptions {
                restore_paused: Some("resume".into()),
                ..TransferOptions::default()
            },
        };
        timed(
            "transfer",
            self.connect()?
                .spclient()
                .transfer(from, device_id, Some(&request)),
        )
        .await?;

        // Spotify doesn't always hand over the play state and position of a librespot
        // device, so check where playback ended up and put it right.
        for _ in 0..TRANSFER_CHECKS {
            tokio::time::sleep(TRANSFER_CHECK_INTERVAL).await;
            let cluster = self.cluster(PutStateReason::NEW_DEVICE).await?;
            if cluster.active_device_id != device_id {
                continue;
            }
            let Some(state) = playback_from_cluster(&cluster) else {
                continue;
            };
            let duration = state.track.as_ref().map_or(0, |t| t.duration_ms);
            if let Some(position) = position_ms {
                let expected = position + started.elapsed().as_millis() as u32;
                if expected < duration && state.progress_ms.abs_diff(expected) > POSITION_SLACK_MS {
                    debug!("transfer lost the position, seeking to {expected} ms");
                    self.command_to(
                        device_id,
                        "seek_to",
                        json!({"value": expected, "position": 0}),
                    )
                    .await?;
                }
            }
            if !state.is_playing {
                debug!("transfer arrived paused, resuming");
                self.command_to(device_id, "resume", json!({})).await?;
            }
            return Ok(());
        }
        Ok(())
    }
}

/// Runs one spclient request with a deadline and a readable error.
async fn timed<T, E>(what: &str, request: impl Future<Output = Result<T, E>>) -> Result<T>
where
    E: Into<anyhow::Error>,
{
    match tokio::time::timeout(REQUEST_TIMEOUT, request).await {
        Ok(result) => result
            .map_err(Into::into)
            .with_context(|| format!("loading {what} from Spotify")),
        Err(_) => bail!("Spotify did not answer in time ({what})"),
    }
}

fn next_page(pages: &[ContextPage]) -> Option<String> {
    pages
        .last()
        .and_then(|p| p.next_page_url.clone())
        .filter(|u| !u.is_empty())
}

/// `spotify:search:` URI that context-resolve turns into search results. Words are joined
/// with `+` and everything else is percent-encoded so the query stays inside the URL path.
pub fn search_uri(query: &str) -> String {
    let words: Vec<String> = query.split_whitespace().map(percent_encode).collect();
    format!("spotify:search:{}", words.join("+"))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The user's playlists from a rootlist, in sidebar order, without folder markers.
pub fn parse_rootlist(list: &SelectedListContent) -> Vec<Playlist> {
    let contents = &*list.contents;
    contents
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let id = item.uri().strip_prefix("spotify:playlist:")?;
            let meta = contents.meta_items.get(i);
            let name = meta
                .map(|m| m.attributes.name())
                .filter(|n| !n.is_empty())
                .unwrap_or("Untitled playlist");
            Some(Playlist {
                id: id.to_string(),
                uri: item.uri().to_string(),
                name: name.to_string(),
                snapshot_id: meta
                    .and_then(|m| m.revision.as_deref())
                    .map(hex)
                    .unwrap_or_default(),
                total: meta.and_then(|m| m.length).unwrap_or(0).max(0) as u32,
            })
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// URIs of the tracks and episodes in the loaded pages of a context, in order.
pub fn playable_uris(pages: &[ContextPage]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|p| &p.tracks)
        .map(|t| t.uri())
        .filter(|u| u.starts_with("spotify:track:") || u.starts_with("spotify:episode:"))
        .map(str::to_string)
        .collect()
}

pub fn track_from_metadata(uri: &str, t: &proto::metadata::Track) -> Track {
    Track {
        uri: uri.to_string(),
        name: t.name().to_string(),
        artists: t
            .artist
            .iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(", "),
        album: t.album.name().to_string(),
        year: t.album.date.year.map(|y| y.to_string()),
        duration_ms: t.duration().max(0) as u32,
    }
}

pub fn episode_from_metadata(uri: &str, e: &proto::metadata::Episode) -> Track {
    Track {
        uri: uri.to_string(),
        name: e.name().to_string(),
        artists: e.show.name().to_string(),
        album: e.show.name().to_string(),
        year: e.publish_time.year.map(|y| y.to_string()),
        duration_ms: e.duration().max(0) as u32,
    }
}

/// Devices that can be picked in the Connect menu, sorted by name.
pub fn devices_from_cluster(cluster: &Cluster) -> Vec<Device> {
    let mut devices: Vec<Device> = cluster
        .device
        .iter()
        .filter(|(_, d)| {
            !d.capabilities.hidden && d.device_type.enum_value() != Ok(DeviceType::OBSERVER)
        })
        .map(|(id, d)| device(cluster, id, d))
        .collect();
    devices.sort_by_key(|d| (d.name.to_lowercase(), d.id.clone()));
    devices
}

fn device(cluster: &Cluster, id: &str, d: &DeviceInfo) -> Device {
    Device {
        id: Some(id.to_string()),
        name: d.name.clone(),
        is_active: cluster.active_device_id == id,
        volume: Some(volume_to_percent(d.volume.min(u16::MAX as u32) as u16)),
        kind: device_kind(d.device_type.enum_value_or_default()).to_string(),
    }
}

fn device_kind(kind: DeviceType) -> &'static str {
    match kind {
        DeviceType::COMPUTER => "Computer",
        DeviceType::TABLET => "Tablet",
        DeviceType::SMARTPHONE => "Phone",
        DeviceType::SPEAKER => "Speaker",
        DeviceType::TV => "TV",
        DeviceType::AVR => "Receiver",
        DeviceType::STB => "TV box",
        DeviceType::AUDIO_DONGLE => "Audio dongle",
        DeviceType::GAME_CONSOLE => "Game console",
        DeviceType::CAST_VIDEO | DeviceType::CAST_AUDIO => "Cast",
        DeviceType::AUTOMOBILE | DeviceType::CAR_THING => "Car",
        DeviceType::SMARTWATCH => "Watch",
        DeviceType::CHROMEBOOK => "Chromebook",
        _ => "",
    }
}

/// Playback on the account's active device, or `None` when no device is active. The
/// track only carries its URI and duration; names come from a metadata lookup.
pub fn playback_from_cluster(cluster: &Cluster) -> Option<PlaybackState> {
    let active = cluster.active_device_id.as_str();
    if active.is_empty() {
        return None;
    }
    let ps = &*cluster.player_state;
    let is_playing = ps.is_playing && !ps.is_paused;
    let mut position = ps.position_as_of_timestamp;
    if is_playing {
        position += (cluster.server_timestamp_ms - ps.timestamp).max(0);
    }
    if ps.duration > 0 {
        position = position.min(ps.duration);
    }
    let options = &*ps.options;
    Some(PlaybackState {
        device: cluster
            .device
            .get(active)
            .map(|d| device(cluster, active, d)),
        track: (!ps.track.uri.is_empty()).then(|| Track {
            uri: ps.track.uri.clone(),
            duration_ms: ps.duration.max(0) as u32,
            ..Track::default()
        }),
        is_playing,
        progress_ms: position.max(0) as u32,
        shuffle: options.shuffling_context,
        repeat: if options.repeating_track {
            Repeat::Track
        } else if options.repeating_context {
            Repeat::Context
        } else {
            Repeat::Off
        },
        context_uri: (!ps.context_uri.is_empty()).then(|| ps.context_uri.clone()),
    })
}

/// Body of a `connect-state/v1/player/command` request.
pub fn command_body(endpoint: &str, fields: serde_json::Value) -> serde_json::Value {
    let mut command = serde_json::Map::new();
    command.insert("endpoint".into(), endpoint.into());
    if let serde_json::Value::Object(fields) = fields {
        command.extend(fields);
    }
    command.insert("logging_params".into(), serde_json::json!({}));
    serde_json::json!({ "command": command })
}

#[derive(Deserialize)]
struct RecentlyPlayed {
    #[serde(rename = "playContexts", default)]
    contexts: Vec<RecentContext>,
}

#[derive(Deserialize)]
struct RecentContext {
    #[serde(rename = "lastPlayedTrackUri")]
    last_track: Option<String>,
}

/// The last track played in each recently played context, newest first, without repeats.
pub fn recent_track_uris(json: &[u8]) -> anyhow::Result<Vec<String>> {
    let recent: RecentlyPlayed = serde_json::from_slice(json)?;
    let mut uris: Vec<String> = Vec::new();
    for uri in recent.contexts.into_iter().filter_map(|c| c.last_track) {
        if !uri.is_empty() && !uris.contains(&uri) {
            uris.push(uri);
        }
    }
    Ok(uris)
}

#[cfg(test)]
mod tests {
    use protobuf::{EnumOrUnknown, MessageField};

    use super::*;
    use proto::{
        connect::Capabilities,
        context_track::ContextTrack,
        metadata,
        player::{ContextPlayerOptions, PlayerState, ProvidedTrack},
        playlist4_external::{Item, ListAttributes, ListItems, MetaItem},
    };

    #[test]
    fn search_uri_joins_words_with_plus() {
        assert_eq!(search_uri("dive into me"), "spotify:search:dive+into+me");
    }

    #[test]
    fn search_uri_encodes_characters_that_would_break_the_path() {
        assert_eq!(
            search_uri("  AC/DC & friends? #1 100%  "),
            "spotify:search:AC%2FDC+%26+friends%3F+%231+100%25"
        );
        assert_eq!(search_uri("beyoncé"), "spotify:search:beyonc%C3%A9");
    }

    fn meta(name: Option<&str>, length: i32, revision: &[u8]) -> MetaItem {
        MetaItem {
            attributes: MessageField::some(ListAttributes {
                name: name.map(str::to_string),
                ..Default::default()
            }),
            length: Some(length),
            revision: Some(revision.to_vec()),
            ..Default::default()
        }
    }

    fn item(uri: &str) -> Item {
        Item {
            uri: Some(uri.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn rootlist_lists_playlists_and_skips_folders() {
        let list = SelectedListContent {
            contents: MessageField::some(ListItems {
                items: vec![
                    item("spotify:playlist:aaa"),
                    item("spotify:start-group:f00:Road+trips"),
                    item("spotify:playlist:bbb"),
                    item("spotify:end-group:f00"),
                    item("spotify:playlist:ccc"),
                ],
                meta_items: vec![
                    meta(Some("Mine"), 12, &[0x01, 0xab]),
                    MetaItem::default(),
                    meta(Some("Summer"), 3, &[0xff]),
                    MetaItem::default(),
                    meta(None, 0, &[]),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let playlists = parse_rootlist(&list);
        let summary: Vec<_> = playlists
            .iter()
            .map(|p| {
                (
                    p.id.as_str(),
                    p.uri.as_str(),
                    p.name.as_str(),
                    p.total,
                    p.snapshot_id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("aaa", "spotify:playlist:aaa", "Mine", 12, "01ab"),
                ("bbb", "spotify:playlist:bbb", "Summer", 3, "ff"),
                ("ccc", "spotify:playlist:ccc", "Untitled playlist", 0, ""),
            ]
        );
    }

    fn page(uris: &[&str]) -> ContextPage {
        ContextPage {
            tracks: uris
                .iter()
                .map(|u| ContextTrack {
                    uri: Some(u.to_string()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn playable_uris_keeps_tracks_and_episodes_in_order() {
        let pages = [
            page(&[
                "spotify:track:1",
                "spotify:local:a:b:c:10",
                "spotify:episode:2",
            ]),
            page(&[
                "spotify:delimiter",
                "",
                "spotify:track:3",
                "spotify:track:1",
            ]),
        ];
        assert_eq!(
            playable_uris(&pages),
            [
                "spotify:track:1",
                "spotify:episode:2",
                "spotify:track:3",
                "spotify:track:1"
            ]
        );
    }

    fn artist(name: &str) -> metadata::Artist {
        metadata::Artist {
            name: Some(name.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn track_metadata_becomes_a_track() {
        let t = metadata::Track {
            name: Some("Wish You Were Here".into()),
            artist: vec![artist("Pink Floyd"), artist("Guest")],
            album: MessageField::some(metadata::Album {
                name: Some("Wish You Were Here".into()),
                date: MessageField::some(metadata::Date {
                    year: Some(1975),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            duration: Some(359_000),
            ..Default::default()
        };
        assert_eq!(
            track_from_metadata("spotify:track:x", &t),
            Track {
                uri: "spotify:track:x".into(),
                name: "Wish You Were Here".into(),
                artists: "Pink Floyd, Guest".into(),
                album: "Wish You Were Here".into(),
                year: Some("1975".into()),
                duration_ms: 359_000,
            }
        );
        let bare = track_from_metadata("spotify:track:y", &metadata::Track::default());
        assert_eq!(bare.year, None);
        assert_eq!(bare.duration_ms, 0);
    }

    #[test]
    fn episode_metadata_uses_the_show_as_artist_and_album() {
        let e = metadata::Episode {
            name: Some("Episode 1".into()),
            duration: Some(60_000),
            show: MessageField::some(metadata::Show {
                name: Some("The Show".into()),
                ..Default::default()
            }),
            publish_time: MessageField::some(metadata::Date {
                year: Some(2024),
                ..Default::default()
            }),
            ..Default::default()
        };
        let t = episode_from_metadata("spotify:episode:e", &e);
        assert_eq!(
            (t.name.as_str(), t.artists.as_str(), t.album.as_str()),
            ("Episode 1", "The Show", "The Show")
        );
        assert_eq!((t.year.as_deref(), t.duration_ms), (Some("2024"), 60_000));
    }

    fn device(name: &str, kind: DeviceType, volume: u32, hidden: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            device_type: EnumOrUnknown::new(kind),
            volume,
            capabilities: MessageField::some(Capabilities {
                hidden,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn cluster() -> Cluster {
        let mut c = Cluster {
            active_device_id: "tv".into(),
            server_timestamp_ms: 1_000_000,
            ..Default::default()
        };
        c.device.insert(
            "tv".into(),
            device("Tv Renzo", DeviceType::TV, 32768, false),
        );
        c.device.insert(
            "pc".into(),
            device("desktop", DeviceType::COMPUTER, 65535, false),
        );
        c.device
            .insert("hobs_1".into(), device("", DeviceType::OBSERVER, 0, true));
        c
    }

    #[test]
    fn devices_come_from_the_cluster_without_hidden_ones() {
        let devices = devices_from_cluster(&cluster());
        assert_eq!(
            devices,
            [
                Device {
                    id: Some("pc".into()),
                    name: "desktop".into(),
                    is_active: false,
                    volume: Some(100),
                    kind: "Computer".into(),
                },
                Device {
                    id: Some("tv".into()),
                    name: "Tv Renzo".into(),
                    is_active: true,
                    volume: Some(50),
                    kind: "TV".into(),
                },
            ]
        );
    }

    fn playing(c: &mut Cluster, is_paused: bool) {
        c.player_state = MessageField::some(PlayerState {
            track: MessageField::some(ProvidedTrack {
                uri: "spotify:track:t".into(),
                ..Default::default()
            }),
            context_uri: "spotify:playlist:p".into(),
            is_playing: true,
            is_paused,
            // 10 s into the track at server time 990 000, i.e. 10 s ago.
            position_as_of_timestamp: 10_000,
            timestamp: 990_000,
            duration: 200_000,
            options: MessageField::some(ContextPlayerOptions {
                shuffling_context: true,
                repeating_context: true,
                repeating_track: false,
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    #[test]
    fn no_active_device_means_nothing_is_playing() {
        let mut c = cluster();
        playing(&mut c, false);
        c.active_device_id.clear();
        assert_eq!(playback_from_cluster(&c), None);
    }

    #[test]
    fn playback_advances_the_position_to_server_time() {
        let mut c = cluster();
        playing(&mut c, false);
        let state = playback_from_cluster(&c).unwrap();
        assert!(state.is_playing);
        assert_eq!(state.progress_ms, 20_000);
        assert_eq!(
            state.track.as_ref().map(|t| t.uri.as_str()),
            Some("spotify:track:t")
        );
        assert_eq!(state.track.as_ref().map(|t| t.duration_ms), Some(200_000));
        assert_eq!(
            state.device.as_ref().map(|d| d.name.as_str()),
            Some("Tv Renzo")
        );
        assert!((state.shuffle, state.repeat) == (true, Repeat::Context));
        assert_eq!(state.context_uri.as_deref(), Some("spotify:playlist:p"));
    }

    #[test]
    fn paused_playback_keeps_its_position() {
        let mut c = cluster();
        playing(&mut c, true);
        let state = playback_from_cluster(&c).unwrap();
        assert!(!state.is_playing);
        assert_eq!(state.progress_ms, 10_000);
    }

    #[test]
    fn command_body_wraps_the_endpoint_and_its_fields() {
        let body = command_body("seek_to", serde_json::json!({"value": 1234, "position": 0}));
        assert_eq!(
            body,
            serde_json::json!({"command": {
                "endpoint": "seek_to",
                "value": 1234,
                "position": 0,
                "logging_params": {}
            }})
        );
        assert_eq!(
            command_body("pause", serde_json::json!({})),
            serde_json::json!({"command": {"endpoint": "pause", "logging_params": {}}})
        );
    }

    #[test]
    fn recent_tracks_are_the_last_track_of_each_context() {
        let json = br#"{"playContexts":[
            {"uri":"spotify:playlist:a","lastPlayedTime":3,"lastPlayedTrackUri":"spotify:track:1"},
            {"uri":"spotify:album:b","lastPlayedTime":2,"lastPlayedTrackUri":"spotify:track:2"},
            {"uri":"spotify:artist:c","lastPlayedTime":1},
            {"uri":"spotify:playlist:d","lastPlayedTime":0,"lastPlayedTrackUri":"spotify:track:1"}
        ]}"#;
        assert_eq!(
            recent_track_uris(json).unwrap(),
            ["spotify:track:1", "spotify:track:2"]
        );
    }
}

/// Checks against the real Spotify account whose login is stored in the OS keyring. They
/// never run by default; run them with
/// `cargo test live_ -- --ignored --nocapture --test-threads=1`.
#[cfg(test)]
mod live {
    use std::time::Instant;

    use super::*;
    use crate::{
        config::Config,
        spotify::{
            observer::{self, ClusterUpdates},
            player::LocalPlayer,
            sink::SampleTap,
        },
    };

    async fn start() -> (LocalPlayer, Internal, ClusterUpdates) {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(std::env::var("TALYXEL_LOG").unwrap_or_else(|_| "warn".into()))
            .with_test_writer()
            .try_init();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let token = crate::auth::obtain_token()
            .await
            .expect("stored Spotify login");
        let cfg = Config {
            initial_volume: 30,
            ..Config::default()
        };
        let (player, _events) = LocalPlayer::start(&cfg, &token.access_token, SampleTap::default())
            .await
            .expect("Premium session");
        let (connect, updates) = observer::start(&token.access_token)
            .await
            .expect("Connect observer");
        let internal = Internal::new(player.session.clone()).with_observer(connect);
        (player, internal, updates)
    }

    /// Waits until the observer has its connection and can read the cluster.
    async fn devices(api: &Internal) -> Vec<Device> {
        for _ in 0..30 {
            if let Ok(devices) = api.devices().await {
                return devices;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        api.devices().await.expect("Connect devices")
    }

    fn show(tracks: &[Track]) -> String {
        tracks
            .iter()
            .take(3)
            .map(|t| format!("\"{}\" by {} ({})", t.name, t.artists, t.album))
            .collect::<Vec<_>>()
            .join("; ")
    }

    #[tokio::test]
    #[ignore = "uses the real Spotify account"]
    async fn live_library_and_search() {
        let (player, api, _) = start().await;

        let t = Instant::now();
        let name = api.display_name().await.unwrap();
        println!("display name: {name} ({:?})", t.elapsed());

        let t = Instant::now();
        let playlists = api.playlists().await.unwrap();
        println!("playlists: {} ({:?})", playlists.len(), t.elapsed());
        for p in playlists.iter().take(5) {
            println!("  - {} ({} tracks)", p.name, p.total);
        }
        assert!(!playlists.is_empty());

        for p in playlists.iter().take(3).chain(
            playlists
                .iter()
                .filter(|p| p.id.starts_with("37i9dQZF1"))
                .take(1),
        ) {
            let t = Instant::now();
            let tracks = api.playlist_tracks(&p.id).await.unwrap();
            println!(
                "playlist {:?}: {} of {} tracks ({:?}): {}",
                p.name,
                tracks.len(),
                p.total,
                t.elapsed(),
                show(&tracks)
            );
            assert!(!tracks.is_empty() || p.total == 0, "{} is empty", p.name);
        }

        let t = Instant::now();
        let liked = api.liked_tracks().await.unwrap();
        println!(
            "liked songs: {} ({:?}): {}",
            liked.len(),
            t.elapsed(),
            show(&liked)
        );
        assert!(!liked.is_empty());
        let head = api.liked_head().await.unwrap();
        assert_eq!(head.0 as usize, liked.len());
        assert_eq!(head.1.as_deref(), liked.first().map(|t| t.uri.as_str()));

        for q in [
            "dive into me",
            "AC/DC",
            "beyoncé",
            "rock & roll",
            "100% pure",
            "a",
        ] {
            let t = Instant::now();
            let results = api.search(q).await.unwrap();
            println!(
                "search {q:?}: {} results ({:?}): {}",
                results.len(),
                t.elapsed(),
                show(&results)
            );
            assert!(!results.is_empty(), "no results for {q:?}");
        }

        let t = Instant::now();
        let recent = api.recently_played().await.unwrap();
        println!(
            "recently played: {} ({:?}): {}",
            recent.len(),
            t.elapsed(),
            show(&recent)
        );
        assert!(!recent.is_empty());

        player.shutdown();
    }

    #[tokio::test]
    #[ignore = "uses the real Spotify account"]
    async fn live_connect_devices_and_playback() {
        let (player, api, _) = start().await;
        let devices = devices(&api).await;
        for d in &devices {
            println!(
                "device: {} [{}] vol {:?} active={} this={}",
                d.name,
                d.kind,
                d.volume,
                d.is_active,
                d.id.as_deref() == Some(api.device_id())
            );
        }
        assert!(
            devices
                .iter()
                .any(|d| d.id.as_deref() == Some(api.device_id())),
            "this device is missing from the list"
        );
        let t = Instant::now();
        let state = api.playback().await.unwrap();
        println!("playback ({:?}): {state:#?}", t.elapsed());
        player.shutdown();
    }

    /// Regression test: the observer must not make Spotify echo this device's own updates
    /// back to it, or librespot answers each echo with another update, several per second.
    #[tokio::test]
    #[ignore = "plays music on this computer"]
    async fn live_playing_does_not_start_an_update_storm() {
        let (player, api, mut updates) = start().await;
        let _ = devices(&api).await;
        let liked = api.liked_tracks().await.unwrap();
        player.load_tracks(&[liked[0].uri.clone()], 0).unwrap();
        let me = api.device_id().to_string();
        let mut ours = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while let Ok(Some(update)) = tokio::time::timeout_at(deadline, updates.next()).await {
            if update.is_ok_and(|u| u.devices_that_changed.contains(&me)) {
                ours += 1;
            }
        }
        let state = api.playback().await.unwrap();
        player.play_pause().unwrap();
        player.shutdown();
        println!("updates from this device in 8 s of playback: {ours}");
        let state = state.expect("playing here");
        assert_eq!(state.device.and_then(|d| d.id), Some(me));
        assert!(state.is_playing);
        assert!(ours <= 8, "{ours} updates in 8 s: the echo loop is back");
    }

    /// Moves playback to the device named in `TALYXEL_LIVE_TARGET` and back. It plays
    /// music there, so it only runs when that variable is set.
    #[tokio::test]
    #[ignore = "plays music on another of your devices"]
    async fn live_transfer_keeps_playing() {
        let Ok(target_name) = std::env::var("TALYXEL_LIVE_TARGET") else {
            println!("set TALYXEL_LIVE_TARGET to a device name to run this");
            return;
        };
        let (player, api, _) = start().await;
        let target = devices(&api)
            .await
            .into_iter()
            .find(|d| d.name == target_name)
            .expect("target device is online");
        let target_id = target.id.clone().unwrap();

        let liked = api.liked_tracks().await.unwrap();
        // Spotify only transfers playback that has a context.
        player
            .load_context(&api.liked_uri(), &liked[0].uri)
            .unwrap();
        // Long enough for the position to be reported, as in real use.
        tokio::time::sleep(Duration::from_secs(15)).await;

        let here = api.playback().await.unwrap().expect("playing here");
        let t = Instant::now();
        api.transfer(&target_id, Some(here.progress_ms))
            .await
            .unwrap();
        let mut state = None;
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = api.playback().await.unwrap().expect("something plays");
            println!(
                "{:?} after transfer: on {:?}, playing={}, at {} ms",
                t.elapsed(),
                now.device.as_ref().map(|d| &d.name),
                now.is_playing,
                now.progress_ms
            );
            let arrived = now.device.as_ref().and_then(|d| d.id.as_deref()) == Some(&target_id);
            let playing = now.is_playing;
            state = Some(now);
            if arrived && playing {
                break;
            }
        }
        let state = state.unwrap();
        let took = t.elapsed();
        // It has to stay playing, not just start.
        for i in 0..8 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = api.playback().await.unwrap();
            println!(
                "  +{}s: on {:?}, playing={:?}, at {:?} ms",
                i + 1,
                now.as_ref()
                    .and_then(|s| s.device.as_ref())
                    .map(|d| d.name.clone()),
                now.as_ref().map(|s| s.is_playing),
                now.as_ref().map(|s| s.progress_ms)
            );
        }
        let later = api.playback().await.unwrap().expect("something plays");
        // Leave the other device paused again.
        api.pause().await.unwrap();
        player.shutdown();
        assert_eq!(state.device.and_then(|d| d.id), Some(target_id));
        assert!(state.is_playing, "playback arrived paused");
        assert!(later.is_playing, "playback stopped after the move");
        // It carries on where it was, give or take the time the move took.
        let expected = here.progress_ms + took.as_millis() as u32;
        assert!(
            state.progress_ms.abs_diff(expected) < 4000,
            "arrived at {} ms, expected about {expected} ms",
            state.progress_ms
        );
    }
}
