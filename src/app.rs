//! Application state and the central event loop.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use librespot_metadata::audio::UniqueFields;
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use ratatui::{DefaultTerminal, widgets::ListState};
use tokio::sync::{Notify, mpsc};
use tracing::warn;

use crate::{
    config::Config,
    event::{AppEvent, MediaCommand, View},
    media_keys::MediaKeys,
    spotify::{
        Device, PlaybackState, Playlist, Repeat, Track, api::Api, internal::search_uri,
        player::LocalPlayer, sink::SampleTap,
    },
    ui, updater,
    visualizer::Spectrum,
};

const SEEK_STEP_MS: u32 = 5_000;
const VOLUME_STEP: u8 = 5;
const STATUS_TTL: Duration = Duration::from_secs(4);

/// Entries above the playlists in the sidebar.
pub const BROWSE: &[&str] = &["Search", "Liked Songs", "Recently Played", "Devices"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Tracks,
}

pub enum Overlay {
    None,
    Search {
        input: String,
    },
    Help,
    Devices {
        devices: Vec<Device>,
        state: ListState,
        loading: bool,
        error: Option<String>,
    },
}

/// What to hand the local player when a track in the list is picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayRequest {
    /// Play a whole context (playlist, Liked Songs, search results) from this track.
    Context { uri: String, track_uri: String },
    /// Play the listed tracks, starting at `index`.
    Tracks { uris: Vec<String>, index: usize },
}

/// Chooses how to play `tracks[index]` from `view`. Contexts start at the track's URI, not
/// its position: the list leaves out items that can't be played, so positions can differ.
pub fn play_request(
    view: &View,
    tracks: &[Track],
    index: usize,
    liked_uri: Option<&str>,
) -> Option<PlayRequest> {
    let track_uri = tracks.get(index)?.uri.clone();
    // Spotify only moves playback to another device when it has a context.
    let context = match view {
        View::Playlist { uri, .. } => Some(uri.clone()),
        View::Liked => liked_uri.map(str::to_string),
        View::Search(query) => Some(search_uri(query)),
        _ => None,
    };
    Some(match context {
        Some(uri) => PlayRequest::Context { uri, track_uri },
        None => PlayRequest::Tracks {
            uris: tracks.iter().map(|t| t.uri.clone()).collect(),
            index,
        },
    })
}

/// What is playing right now, on whichever device.
#[derive(Debug, Clone, Default)]
pub struct NowPlaying {
    pub track: Option<Track>,
    pub is_playing: bool,
    pub position_ms: u32,
    pub position_at: Option<Instant>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub volume: u8,
    pub device_name: Option<String>,
    pub context_uri: Option<String>,
}

impl NowPlaying {
    /// Position interpolated since the last report.
    pub fn position(&self) -> u32 {
        let base = self.position_ms;
        let pos = match (self.is_playing, self.position_at) {
            (true, Some(at)) => base.saturating_add(at.elapsed().as_millis() as u32),
            _ => base,
        };
        match &self.track {
            Some(t) if t.duration_ms > 0 => pos.min(t.duration_ms),
            _ => pos,
        }
    }

    fn set_position(&mut self, ms: u32) {
        self.position_ms = ms;
        self.position_at = Some(Instant::now());
    }
}

pub struct App {
    pub cfg: Config,
    pub api: Api,
    pub local: Option<LocalPlayer>,
    /// Spotify Connect id of this app's device, when local playback is available.
    pub device_id: Option<String>,
    pub local_active: Arc<AtomicBool>,
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub poll_wake: Arc<Notify>,

    pub username: Option<String>,
    pub playlists: Vec<Playlist>,
    /// Whether the playlists have been loaded at least once.
    pub playlists_loaded: bool,
    /// Why the last attempt to load the playlists failed.
    pub playlists_error: Option<String>,
    pub sidebar: ListState,
    pub view: View,
    pub tracks: Vec<Track>,
    pub track_state: ListState,
    pub loading: bool,
    /// Why the current view could not be loaded.
    pub view_error: Option<String>,
    pub focus: Focus,
    pub overlay: Overlay,

    pub now: NowPlaying,
    pub tap: SampleTap,
    pub spectrum: Spectrum,
    pub spectrum_width: usize,
    started: Instant,

    pub status: Option<(String, bool, Instant)>,
    pub update_available: Option<String>,
    pub updating: bool,
    media: Option<MediaKeys>,
    /// Offline preview with mock data (`talyxel demo`); no Spotify calls are made.
    pub demo: bool,
    pub should_quit: bool,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: Config,
        api: Api,
        local: Option<LocalPlayer>,
        tap: SampleTap,
        local_active: Arc<AtomicBool>,
        poll_wake: Arc<Notify>,
        tx: mpsc::UnboundedSender<AppEvent>,
        media: Option<MediaKeys>,
    ) -> Self {
        let mut sidebar = ListState::default();
        sidebar.select(Some(1));
        let volume = cfg.initial_volume;
        let device_id = local.as_ref().map(|l| l.session.device_id().to_string());
        Self {
            cfg,
            api,
            local,
            device_id,
            local_active,
            tx,
            poll_wake,
            username: None,
            playlists: Vec::new(),
            playlists_loaded: false,
            playlists_error: None,
            sidebar,
            view: View::Empty,
            tracks: Vec::new(),
            track_state: ListState::default(),
            loading: false,
            view_error: None,
            focus: Focus::Sidebar,
            overlay: Overlay::None,
            now: NowPlaying {
                volume,
                ..NowPlaying::default()
            },
            tap,
            spectrum: Spectrum::default(),
            spectrum_width: 0,
            started: Instant::now(),
            status: None,
            update_available: None,
            updating: false,
            media,
            demo: false,
            should_quit: false,
        }
    }

    pub fn is_local(&self) -> bool {
        self.local.is_some() && self.local_active.load(Ordering::Relaxed)
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false, Instant::now()));
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), true, Instant::now() + Duration::from_secs(4)));
    }

    pub fn visible_status(&self) -> Option<(&str, bool)> {
        self.status
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < STATUS_TTL || *at > Instant::now())
            .map(|(m, e, _)| (m.as_str(), *e))
    }

    /// Runs an async Web API call in the background; errors show in the status bar.
    fn spawn_api<F, Fut>(&self, f: F)
    where
        F: FnOnce(Api) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<AppEvent>>> + Send,
    {
        let api = self.api.clone();
        let tx = self.tx.clone();
        let wake = self.poll_wake.clone();
        tokio::spawn(async move {
            match f(api).await {
                Ok(Some(ev)) => {
                    let _ = tx.send(ev);
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("{e:#}")));
                }
            }
            wake.notify_one();
        });
    }

    pub fn start(&mut self) {
        debug_assert!(!self.demo);
        self.spawn_api(|api| async move {
            let name = api.username().await?;
            Ok(Some(AppEvent::Status(format!("Logged in as {name}"))))
        });
        self.load_view(View::Liked);
    }

    fn load_view(&mut self, view: View) {
        if self.demo {
            self.view = view;
            return;
        }
        self.loading = true;
        self.view_error = None;
        self.view = view.clone();
        self.tracks.clear();
        self.track_state.select(None);
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = match &view {
                View::Empty => Ok(Vec::new()),
                View::Liked => api.liked_tracks().await,
                View::Recent => api.recently_played().await,
                View::Search(q) => api.search(q).await,
                View::Playlist { id, .. } => api.playlist_tracks(id).await,
            };
            let _ = tx.send(match result {
                Ok(tracks) => AppEvent::Tracks { view, tracks },
                Err(e) => AppEvent::TracksFailed {
                    view,
                    error: format!("{e:#}"),
                },
            });
        });
    }

    fn refresh_devices(&mut self) {
        if let Overlay::Devices { loading, error, .. } = &mut self.overlay {
            *loading = true;
            *error = None;
        }
        if self.demo {
            let _ = self.tx.send(AppEvent::Devices(crate::demo::devices()));
            return;
        }
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(match api.devices().await {
                Ok(devices) => AppEvent::Devices(devices),
                Err(e) => AppEvent::DevicesFailed(format!("{e:#}")),
            });
        });
    }

    /// Whether `device` is this app's own Connect device.
    pub fn is_this_device(&self, device: &Device) -> bool {
        match (&self.device_id, &device.id) {
            (Some(me), Some(id)) => me == id,
            // Without ids (Web API only), fall back to the configured name.
            _ => device.name == self.cfg.device_name,
        }
    }

    // ---------------------------------------------------------------- events

    pub fn on_tick(&mut self) {
        let width = self.spectrum_width;
        if self.is_local() && self.now.is_playing {
            self.spectrum.update(&self.tap, width);
        } else if self.now.is_playing {
            self.spectrum
                .idle(self.started.elapsed().as_secs_f32(), width);
        } else {
            self.spectrum.decay(width);
        }
    }

    pub fn on_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Remote(state) => self.on_remote(state),
            AppEvent::Playlists {
                playlists,
                changed,
                initial,
            } => {
                self.playlists = playlists;
                self.playlists_loaded = true;
                self.playlists_error = None;
                let max = BROWSE.len() + self.playlists.len();
                if self.sidebar.selected().is_some_and(|i| i >= max) {
                    self.sidebar.select(Some(max.saturating_sub(1)));
                }
                if !initial {
                    self.set_status("Library updated");
                    if let View::Playlist { id, .. } = &self.view
                        && changed.contains(id)
                    {
                        match self.playlists.iter().find(|p| &p.id == id) {
                            Some(p) => self.reload_view(View::Playlist {
                                id: p.id.clone(),
                                uri: p.uri.clone(),
                                name: p.name.clone(),
                            }),
                            None => self.view = View::Empty,
                        }
                    }
                }
            }
            AppEvent::LikedChanged => {
                self.set_status("Liked Songs updated");
                if self.view == View::Liked {
                    self.reload_view(View::Liked);
                }
            }
            AppEvent::Tracks { view, tracks } => {
                if view == self.view {
                    let keep = self.track_state.selected();
                    self.tracks = tracks;
                    self.loading = false;
                    self.view_error = None;
                    let sel = keep
                        .filter(|&i| i < self.tracks.len())
                        .or((!self.tracks.is_empty()).then_some(0));
                    self.track_state.select(sel);
                }
            }
            AppEvent::Devices(mut devices) => {
                // This device first, then the others in Spotify's order.
                devices.sort_by_key(|d| !self.is_this_device(d));
                if let Overlay::Devices {
                    devices: d,
                    state,
                    loading,
                    error,
                } = &mut self.overlay
                {
                    let kept = state
                        .selected()
                        .and_then(|i| d.get(i))
                        .and_then(|x| x.id.clone());
                    *d = devices;
                    *loading = false;
                    *error = None;
                    let selected = kept
                        .and_then(|id| d.iter().position(|x| x.id.as_ref() == Some(&id)))
                        .or_else(|| d.iter().position(|x| x.is_active))
                        .or((!d.is_empty()).then_some(0));
                    state.select(selected);
                }
            }
            AppEvent::PlaylistsFailed(e) => {
                if !self.playlists_loaded {
                    self.set_error(format!("Could not load playlists: {e}"));
                }
                self.playlists_error = Some(e);
            }
            AppEvent::DevicesFailed(e) => {
                if let Overlay::Devices { loading, error, .. } = &mut self.overlay {
                    *loading = false;
                    *error = Some(e);
                } else {
                    self.set_error(e);
                }
            }
            AppEvent::TracksFailed { view, error } => {
                if view == self.view {
                    self.loading = false;
                    self.view_error = Some(error.clone());
                    self.set_error(error);
                }
            }
            AppEvent::Media(cmd) => match cmd {
                MediaCommand::Play if !self.now.is_playing => self.toggle_play(),
                MediaCommand::Pause if self.now.is_playing => self.toggle_play(),
                MediaCommand::Toggle => self.toggle_play(),
                MediaCommand::Next => self.next(),
                MediaCommand::Previous => self.prev(),
                _ => {}
            },
            AppEvent::UpdateAvailable(v) => self.update_available = Some(v),
            AppEvent::UpdateInstalled(v) => {
                self.updating = false;
                self.update_available = None;
                self.set_status(format!("Updated to v{v} — restart Talyxel Sound to use it"));
            }
            AppEvent::Status(s) => {
                if let Some(name) = s.strip_prefix("Logged in as ") {
                    self.username = Some(name.to_string());
                }
                self.set_status(s);
            }
            AppEvent::Error(e) => {
                self.updating = false;
                self.set_error(e);
            }
        }
    }

    /// Re-fetches a view without clearing it, so the list doesn't flicker.
    fn reload_view(&mut self, view: View) {
        self.view = view.clone();
        self.spawn_api(move |api| async move {
            let tracks = match &view {
                View::Liked => api.liked_tracks().await?,
                View::Playlist { id, .. } => api.playlist_tracks(id).await?,
                _ => return Ok(None),
            };
            Ok(Some(AppEvent::Tracks { view, tracks }))
        });
    }

    fn on_remote(&mut self, state: Option<PlaybackState>) {
        let Some(state) = state else {
            if !self.is_local() {
                self.now.is_playing = false;
                self.now.device_name = None;
            }
            return;
        };
        let other_device_active = state
            .device
            .as_ref()
            .is_some_and(|d| d.is_active && !self.is_this_device(d));
        if other_device_active {
            self.local_active.store(false, Ordering::Relaxed);
        }
        if self.is_local() {
            // The local player is authoritative for position; take shuffle/repeat from the API.
            self.now.shuffle = state.shuffle;
            self.now.repeat = state.repeat;
            return;
        }
        let track_changed = self.now.track != state.track;
        self.now.track = state.track;
        self.now.is_playing = state.is_playing;
        self.now.set_position(state.progress_ms);
        self.now.shuffle = state.shuffle;
        self.now.repeat = state.repeat;
        self.now.context_uri = state.context_uri;
        if let Some(d) = state.device {
            if let Some(v) = d.volume {
                self.now.volume = v;
            }
            self.now.device_name = Some(d.name);
        }
        if track_changed {
            self.push_media_metadata();
        }
        self.push_media_playback();
    }

    pub fn on_player_event(&mut self, ev: PlayerEvent) {
        // After playback moved to another device, librespot still pauses and stops the
        // local player; that must not overwrite what the other device is doing.
        let stale = !self.is_local()
            && matches!(
                ev,
                PlayerEvent::Paused { .. }
                    | PlayerEvent::Stopped { .. }
                    | PlayerEvent::Seeked { .. }
                    | PlayerEvent::PositionCorrection { .. }
                    | PlayerEvent::PositionChanged { .. }
                    | PlayerEvent::VolumeChanged { .. }
                    | PlayerEvent::ShuffleChanged { .. }
                    | PlayerEvent::RepeatChanged { .. }
            );
        if stale {
            return;
        }
        match ev {
            PlayerEvent::SessionConnected { .. } => {
                self.local_active.store(true, Ordering::Relaxed);
                self.now.device_name = Some(self.cfg.device_name.clone());
            }
            PlayerEvent::SessionDisconnected { .. } => {
                self.local_active.store(false, Ordering::Relaxed);
                self.poll_wake.notify_one();
            }
            PlayerEvent::TrackChanged { audio_item } => {
                self.local_active.store(true, Ordering::Relaxed);
                let (artists, album) = match &audio_item.unique_fields {
                    UniqueFields::Track { artists, album, .. } => (
                        artists
                            .iter()
                            .map(|a| a.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        album.clone(),
                    ),
                    UniqueFields::Episode { show_name, .. } => (show_name.clone(), String::new()),
                    UniqueFields::Local { artists, album, .. } => (
                        artists.clone().unwrap_or_default(),
                        album.clone().unwrap_or_default(),
                    ),
                };
                let year = self
                    .tracks
                    .iter()
                    .find(|t| t.uri == audio_item.uri)
                    .and_then(|t| t.year.clone());
                self.now.track = Some(Track {
                    uri: audio_item.uri.clone(),
                    name: audio_item.name.clone(),
                    artists,
                    album,
                    year,
                    duration_ms: audio_item.duration_ms,
                });
                self.now.device_name = Some(self.cfg.device_name.clone());
                self.push_media_metadata();
            }
            PlayerEvent::Playing { position_ms, .. } => {
                self.local_active.store(true, Ordering::Relaxed);
                self.now.is_playing = true;
                self.now.set_position(position_ms);
                self.push_media_playback();
            }
            PlayerEvent::Paused { position_ms, .. } => {
                self.now.is_playing = false;
                self.now.set_position(position_ms);
                self.push_media_playback();
            }
            PlayerEvent::Stopped { .. } => {
                self.now.is_playing = false;
                self.push_media_playback();
            }
            PlayerEvent::Seeked { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. }
            | PlayerEvent::PositionChanged { position_ms, .. } => {
                self.now.set_position(position_ms);
                self.push_media_playback();
            }
            PlayerEvent::VolumeChanged { volume } => {
                self.now.volume = crate::spotify::player::volume_to_percent(volume);
            }
            PlayerEvent::ShuffleChanged { shuffle } => self.now.shuffle = shuffle,
            PlayerEvent::RepeatChanged { context, track } => {
                self.now.repeat = if track {
                    Repeat::Track
                } else if context {
                    Repeat::Context
                } else {
                    Repeat::Off
                };
            }
            PlayerEvent::Unavailable { .. } => self.set_error("Track unavailable"),
            _ => {}
        }
    }

    fn push_media_metadata(&self) {
        if let (Some(media), Some(t)) = (&self.media, &self.now.track) {
            media.set_metadata(&t.name, &t.artists, &t.album, t.duration_ms);
        }
    }

    fn push_media_playback(&self) {
        if let Some(media) = &self.media {
            media.set_playback(self.now.is_playing, self.now.position());
        }
    }

    // ---------------------------------------------------------------- controls

    fn local_or_remote(
        &mut self,
        local: impl FnOnce(&LocalPlayer) -> Result<()>,
        remote: impl FnOnce(Api) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + 'static,
    ) {
        if self.demo {
            return;
        }
        if self.is_local() {
            if let Some(p) = &self.local
                && let Err(e) = local(p)
            {
                self.set_error(format!("{e:#}"));
            }
        } else if self.now.device_name.is_some() {
            self.spawn_api(move |api| async move {
                remote(api).await?;
                Ok(None)
            });
        } else {
            self.set_status("Nothing is playing — pick a track and press Enter");
        }
    }

    fn toggle_play(&mut self) {
        let playing = self.now.is_playing;
        if !self.is_local() && self.now.device_name.is_some() {
            // Optimistic update; the poller confirms.
            self.now.set_position(self.now.position());
            self.now.is_playing = !playing;
        }
        self.local_or_remote(
            |p| p.play_pause(),
            move |api| {
                Box::pin(async move {
                    if playing {
                        api.pause().await
                    } else {
                        api.resume().await
                    }
                })
            },
        );
    }

    fn next(&mut self) {
        self.local_or_remote(
            |p| p.next(),
            |api| Box::pin(async move { api.next().await }),
        );
    }

    fn prev(&mut self) {
        self.local_or_remote(
            |p| p.prev(),
            |api| Box::pin(async move { api.prev().await }),
        );
    }

    fn seek_by(&mut self, delta_ms: i64) {
        let Some(track) = &self.now.track else { return };
        let target =
            (self.now.position() as i64 + delta_ms).clamp(0, track.duration_ms as i64) as u32;
        self.now.set_position(target);
        self.local_or_remote(
            move |p| p.seek(target),
            move |api| Box::pin(async move { api.seek(target).await }),
        );
    }

    fn change_volume(&mut self, delta: i16) {
        let v = (self.now.volume as i16 + delta).clamp(0, 100) as u8;
        self.now.volume = v;
        self.local_or_remote(
            move |p| p.set_volume_percent(v),
            move |api| Box::pin(async move { api.volume(v).await }),
        );
    }

    fn toggle_shuffle(&mut self) {
        let on = !self.now.shuffle;
        self.now.shuffle = on;
        self.local_or_remote(
            move |p| p.shuffle(on),
            move |api| Box::pin(async move { api.shuffle(on).await }),
        );
    }

    fn cycle_repeat(&mut self) {
        let r = self.now.repeat.cycle();
        self.now.repeat = r;
        self.local_or_remote(
            move |p| p.repeat(r != Repeat::Off, r == Repeat::Track),
            move |api| Box::pin(async move { api.repeat(r).await }),
        );
    }

    fn play_selected_track(&mut self) {
        let Some(idx) = self.track_state.selected() else {
            return;
        };
        if idx >= self.tracks.len() {
            return;
        }
        if self.demo {
            self.now.track = Some(self.tracks[idx].clone());
            self.now.is_playing = true;
            self.now.set_position(0);
            return;
        }
        let Some(local) = &self.local else {
            self.set_error("Local playback is unavailable (Spotify Premium is required)");
            return;
        };
        let liked_uri = self.api.internal().map(|i| i.liked_uri());
        let result = match play_request(&self.view, &self.tracks, idx, liked_uri.as_deref()) {
            Some(PlayRequest::Context { uri, track_uri }) => local.load_context(&uri, &track_uri),
            Some(PlayRequest::Tracks { uris, index }) => local.load_tracks(&uris, index),
            None => return,
        };
        match result {
            Ok(()) => {
                self.local_active.store(true, Ordering::Relaxed);
                self.now.track = Some(self.tracks[idx].clone());
                self.now.set_position(0);
                self.now.device_name = Some(self.cfg.device_name.clone());
            }
            Err(e) => self.set_error(format!("{e:#}")),
        }
    }

    fn activate_sidebar(&mut self) {
        let Some(i) = self.sidebar.selected() else {
            return;
        };
        match i {
            0 => {
                self.overlay = Overlay::Search {
                    input: String::new(),
                }
            }
            1 => {
                self.load_view(View::Liked);
                self.focus = Focus::Tracks;
            }
            2 => {
                self.load_view(View::Recent);
                self.focus = Focus::Tracks;
            }
            3 => self.open_devices(),
            n => {
                if let Some(p) = self.playlists.get(n - BROWSE.len()) {
                    let view = View::Playlist {
                        id: p.id.clone(),
                        uri: p.uri.clone(),
                        name: p.name.clone(),
                    };
                    self.load_view(view);
                    self.focus = Focus::Tracks;
                }
            }
        }
    }

    fn open_devices(&mut self) {
        self.overlay = Overlay::Devices {
            devices: Vec::new(),
            state: ListState::default(),
            loading: true,
            error: None,
        };
        self.refresh_devices();
    }

    fn transfer_to(&mut self, device: Device) {
        self.overlay = Overlay::None;
        if device.is_active {
            self.set_status(format!("Already playing on {}", device.name));
            return;
        }
        if self.is_this_device(&device) {
            match &self.local {
                Some(local) => match local.transfer_here() {
                    Ok(()) => self.set_status(format!("Playing on {}", device.name)),
                    Err(e) => self.set_error(format!("{e:#}")),
                },
                None => self.set_error("Local playback is unavailable"),
            }
            return;
        }
        let Some(id) = device.id else { return };
        let name = device.name.clone();
        let position = self.now.track.is_some().then(|| self.now.position());
        self.local_active.store(false, Ordering::Relaxed);
        self.set_status(format!("Transferring playback to {}", device.name));
        self.spawn_api(move |api| async move {
            api.transfer(&id, position).await?;
            Ok(Some(AppEvent::Status(format!("Playing on {name}"))))
        });
    }

    // ---------------------------------------------------------------- input

    pub fn on_input(&mut self, ev: Event) {
        let Event::Key(key) = ev else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        match &mut self.overlay {
            Overlay::None => self.on_key(key),
            Overlay::Help => self.overlay = Overlay::None,
            Overlay::Search { input } => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Enter => {
                    let q = input.trim().to_string();
                    self.overlay = Overlay::None;
                    if !q.is_empty() {
                        self.load_view(View::Search(q));
                        self.focus = Focus::Tracks;
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            },
            Overlay::Devices { devices, state, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => {
                    self.overlay = Overlay::None
                }
                KeyCode::Up | KeyCode::Char('k') => state.select_previous(),
                KeyCode::Down | KeyCode::Char('j') => {
                    let next = state
                        .selected()
                        .map_or(0, |i| (i + 1).min(devices.len().saturating_sub(1)));
                    state.select(Some(next));
                }
                KeyCode::Char('r') => self.refresh_devices(),
                KeyCode::Enter => {
                    if let Some(d) = state.selected().and_then(|i| devices.get(i)).cloned() {
                        self.transfer_to(d);
                    }
                }
                _ => {}
            },
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Char('/') => {
                self.overlay = Overlay::Search {
                    input: String::new(),
                }
            }
            KeyCode::Char('d') => self.open_devices(),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Tracks,
                    Focus::Tracks => Focus::Sidebar,
                }
            }
            KeyCode::Char('h') if self.focus == Focus::Tracks => self.focus = Focus::Sidebar,
            KeyCode::Char('l') if self.focus == Focus::Sidebar => self.focus = Focus::Tracks,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(i32::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(i32::MAX / 2),
            KeyCode::Enter => match self.focus {
                Focus::Sidebar => self.activate_sidebar(),
                Focus::Tracks => self.play_selected_track(),
            },
            KeyCode::Char(' ') => self.toggle_play(),
            KeyCode::Char('n') | KeyCode::Char('>') => self.next(),
            KeyCode::Char('p') | KeyCode::Char('<') => self.prev(),
            KeyCode::Left => self.seek_by(-(SEEK_STEP_MS as i64)),
            KeyCode::Right => self.seek_by(SEEK_STEP_MS as i64),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(VOLUME_STEP as i16),
            KeyCode::Char('-') | KeyCode::Char('_') => self.change_volume(-(VOLUME_STEP as i16)),
            KeyCode::Char('s') => self.toggle_shuffle(),
            KeyCode::Char('r') => self.cycle_repeat(),
            KeyCode::Char('U') => {
                if self.updating {
                    return;
                }
                if self.demo {
                    self.set_status("Demo mode: the update would be downloaded and installed here");
                    return;
                }
                match &self.update_available {
                    Some(v) => {
                        self.updating = true;
                        self.set_status(format!("Downloading v{v}…"));
                        updater::spawn_install(self.cfg.clone(), self.tx.clone());
                    }
                    None => self.set_status("No update available"),
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let (state, len) = match self.focus {
            Focus::Sidebar => (&mut self.sidebar, BROWSE.len() + self.playlists.len()),
            Focus::Tracks => (&mut self.track_state, self.tracks.len()),
        };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as i64;
        let next = (cur + delta as i64).clamp(0, len as i64 - 1) as usize;
        state.select(Some(next));
    }

    pub fn shutdown(&self) {
        if let Some(local) = &self.local {
            local.shutdown();
        }
    }
}

pub async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: mpsc::UnboundedReceiver<AppEvent>,
    player_events: Option<PlayerEventChannel>,
) -> Result<()> {
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Keep a sender alive so the placeholder channel never closes.
    let (_keep, placeholder) = mpsc::unbounded_channel();
    let mut player_events = player_events.unwrap_or(placeholder);

    if !app.demo {
        app.start();
    }
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        tokio::select! {
            _ = tick.tick() => app.on_tick(),
            ev = input.next() => match ev {
                Some(Ok(ev)) => app.on_input(ev),
                Some(Err(e)) => warn!("input error: {e}"),
                None => app.should_quit = true,
            },
            Some(ev) = player_events.recv() => app.on_player_event(ev),
            Some(ev) = rx.recv() => app.on_app_event(ev),
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        crate::demo::app(tx)
    }

    fn track(uri: &str) -> Track {
        Track {
            uri: uri.into(),
            ..Track::default()
        }
    }

    fn device(id: &str, name: &str, is_active: bool) -> Device {
        Device {
            id: Some(id.into()),
            name: name.into(),
            is_active,
            volume: None,
            kind: String::new(),
        }
    }

    #[tokio::test]
    async fn a_failed_load_stops_loading_and_keeps_the_reason() {
        let mut app = app();
        app.view = View::Liked;
        app.loading = true;
        app.tracks.clear();
        app.on_app_event(AppEvent::TracksFailed {
            view: View::Liked,
            error: "Spotify did not answer in time".into(),
        });
        assert!(!app.loading);
        assert_eq!(
            app.view_error.as_deref(),
            Some("Spotify did not answer in time")
        );
    }

    #[tokio::test]
    async fn a_failure_for_a_view_that_was_left_is_ignored() {
        let mut app = app();
        app.view = View::Recent;
        app.loading = true;
        app.on_app_event(AppEvent::TracksFailed {
            view: View::Liked,
            error: "old".into(),
        });
        assert!(app.loading);
        assert_eq!(app.view_error, None);
    }

    #[tokio::test]
    async fn loaded_tracks_clear_an_earlier_error() {
        let mut app = app();
        app.view = View::Liked;
        app.view_error = Some("old".into());
        app.on_app_event(AppEvent::Tracks {
            view: View::Liked,
            tracks: vec![track("spotify:track:1")],
        });
        assert_eq!(app.view_error, None);
    }

    #[tokio::test]
    async fn device_errors_show_in_the_open_device_list() {
        let mut app = app();
        app.overlay = Overlay::Devices {
            devices: Vec::new(),
            state: ListState::default(),
            loading: true,
            error: None,
        };
        app.on_app_event(AppEvent::DevicesFailed("no connection".into()));
        let Overlay::Devices { loading, error, .. } = &app.overlay else {
            panic!("device list closed");
        };
        assert!(!loading);
        assert_eq!(error.as_deref(), Some("no connection"));
    }

    #[tokio::test]
    async fn this_device_is_listed_first_and_the_active_one_selected() {
        let mut app = app();
        app.device_id = Some("me".into());
        app.overlay = Overlay::Devices {
            devices: Vec::new(),
            state: ListState::default(),
            loading: true,
            error: None,
        };
        app.on_app_event(AppEvent::Devices(vec![
            device("a", "Kitchen", false),
            device("tv", "Tv", true),
            device("me", "Talyxel Sound", false),
        ]));
        let Overlay::Devices {
            devices,
            state,
            loading,
            ..
        } = &app.overlay
        else {
            panic!("device list closed");
        };
        let ids: Vec<_> = devices.iter().filter_map(|d| d.id.as_deref()).collect();
        assert_eq!(ids, ["me", "a", "tv"]);
        assert_eq!(state.selected(), Some(2));
        assert!(!loading);
    }

    #[tokio::test]
    async fn the_local_player_stopping_after_a_move_keeps_remote_playback_shown() {
        let mut app = app();
        app.device_id = Some("me".into());
        // Playback moved to the TV, so this device is no longer the active one.
        app.local_active.store(false, Ordering::Relaxed);
        app.on_app_event(AppEvent::Remote(Some(PlaybackState {
            device: Some(device("tv", "Tv", true)),
            track: Some(track("spotify:track:1")),
            is_playing: true,
            progress_ms: 10_000,
            ..PlaybackState::default()
        })));
        // librespot then stops the local player.
        let id =
            librespot_core::SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        app.on_player_event(PlayerEvent::Paused {
            play_request_id: 1,
            track_id: id.clone(),
            position_ms: 30_000,
        });
        app.on_player_event(PlayerEvent::Stopped {
            play_request_id: 1,
            track_id: id,
        });
        assert!(app.now.is_playing);
        assert_eq!(app.now.device_name.as_deref(), Some("Tv"));
        assert!(app.now.position() < 20_000);
    }

    #[tokio::test]
    async fn picking_the_device_that_already_plays_just_says_so() {
        let mut app = app();
        let mut state = ListState::default();
        state.select(Some(0));
        app.overlay = Overlay::Devices {
            devices: vec![device("tv", "Tv", true)],
            state,
            loading: false,
            error: None,
        };
        app.on_input(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.visible_status(), Some(("Already playing on Tv", false)));
    }

    #[tokio::test]
    async fn another_device_with_our_name_is_not_this_device() {
        let mut app = app();
        app.device_id = Some("me".into());
        app.local_active.store(true, Ordering::Relaxed);
        app.on_app_event(AppEvent::Remote(Some(PlaybackState {
            device: Some(device("laptop", &app.cfg.device_name.clone(), true)),
            is_playing: true,
            ..PlaybackState::default()
        })));
        assert!(!app.local_active.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn playlist_failures_are_remembered_until_they_load() {
        let mut app = app();
        app.playlists.clear();
        app.playlists_loaded = false;
        app.on_app_event(AppEvent::PlaylistsFailed("offline".into()));
        assert_eq!(app.playlists_error.as_deref(), Some("offline"));
        app.on_app_event(AppEvent::Playlists {
            playlists: Vec::new(),
            changed: Vec::new(),
            initial: true,
        });
        assert!(app.playlists_loaded);
        assert_eq!(app.playlists_error, None);
    }

    #[test]
    fn playlists_play_as_a_context_from_the_picked_track() {
        let view = View::Playlist {
            id: "p".into(),
            uri: "spotify:playlist:p".into(),
            name: "P".into(),
        };
        let tracks = [track("spotify:track:1"), track("spotify:track:2")];
        assert_eq!(
            play_request(&view, &tracks, 1, None),
            Some(PlayRequest::Context {
                uri: "spotify:playlist:p".into(),
                track_uri: "spotify:track:2".into(),
            })
        );
    }

    #[test]
    fn liked_songs_play_as_a_context_when_known() {
        let tracks = [track("spotify:track:1"), track("spotify:track:2")];
        assert_eq!(
            play_request(&View::Liked, &tracks, 0, Some("spotify:user:u:collection")),
            Some(PlayRequest::Context {
                uri: "spotify:user:u:collection".into(),
                track_uri: "spotify:track:1".into(),
            })
        );
        assert_eq!(
            play_request(&View::Liked, &tracks, 1, None),
            Some(PlayRequest::Tracks {
                uris: vec!["spotify:track:1".into(), "spotify:track:2".into()],
                index: 1,
            })
        );
    }

    #[test]
    fn search_results_play_as_their_search_context() {
        // Spotify only transfers playback to another device when it has a context.
        let tracks = [track("spotify:track:1"), track("spotify:track:2")];
        assert_eq!(
            play_request(&View::Search("dive into me".into()), &tracks, 1, None),
            Some(PlayRequest::Context {
                uri: "spotify:search:dive+into+me".into(),
                track_uri: "spotify:track:2".into(),
            })
        );
    }

    #[test]
    fn recently_played_tracks_play_as_a_track_list() {
        let tracks = [track("spotify:track:1"), track("spotify:track:2")];
        assert_eq!(
            play_request(&View::Recent, &tracks, 1, None),
            Some(PlayRequest::Tracks {
                uris: vec!["spotify:track:1".into(), "spotify:track:2".into()],
                index: 1,
            })
        );
        assert_eq!(
            play_request(&View::Search("q".into()), &tracks, 2, None),
            None
        );
    }
}
