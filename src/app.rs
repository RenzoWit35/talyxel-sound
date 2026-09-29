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
        Device, PlaybackState, Playlist, Repeat, Track, api::Api, player::LocalPlayer,
        sink::SampleTap,
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
    },
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
    pub local_active: Arc<AtomicBool>,
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub poll_wake: Arc<Notify>,

    pub username: Option<String>,
    pub playlists: Vec<Playlist>,
    pub sidebar: ListState,
    pub view: View,
    pub tracks: Vec<Track>,
    pub track_state: ListState,
    pub loading: bool,
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
        Self {
            cfg,
            api,
            local,
            local_active,
            tx,
            poll_wake,
            username: None,
            playlists: Vec::new(),
            sidebar,
            view: View::Empty,
            tracks: Vec::new(),
            track_state: ListState::default(),
            loading: false,
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
        self.view = view.clone();
        self.tracks.clear();
        self.track_state.select(None);
        self.spawn_api(move |api| async move {
            let tracks = match &view {
                View::Empty => Vec::new(),
                View::Liked => api.liked_tracks().await?,
                View::Recent => api.recently_played().await?,
                View::Search(q) => api.search(q).await?,
                View::Playlist { id, .. } => api.playlist_tracks(id).await?,
            };
            Ok(Some(AppEvent::Tracks { view, tracks }))
        });
    }

    fn refresh_devices(&self) {
        if self.demo {
            let _ = self.tx.send(AppEvent::Devices(crate::demo::devices()));
            return;
        }
        self.spawn_api(|api| async move { Ok(Some(AppEvent::Devices(api.devices().await?))) });
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
                    let sel = keep
                        .filter(|&i| i < self.tracks.len())
                        .or((!self.tracks.is_empty()).then_some(0));
                    self.track_state.select(sel);
                }
            }
            AppEvent::Devices(devices) => {
                if let Overlay::Devices { devices: d, state } = &mut self.overlay {
                    *d = devices;
                    if state.selected().is_none() && !d.is_empty() {
                        state.select(Some(0));
                    }
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
        let our_name = self.cfg.device_name.clone();
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
            .is_some_and(|d| d.is_active && d.name != our_name);
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
        let result = match &self.view {
            View::Playlist { uri, .. } => local.load(Some(uri), &[], idx),
            _ => {
                let uris: Vec<String> = self.tracks.iter().map(|t| t.uri.clone()).collect();
                local.load(None, &uris, idx)
            }
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
        };
        self.refresh_devices();
    }

    fn transfer_to(&mut self, device: Device) {
        self.overlay = Overlay::None;
        if device.name == self.cfg.device_name {
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
        self.local_active.store(false, Ordering::Relaxed);
        self.set_status(format!("Transferring playback to {}", device.name));
        self.spawn_api(move |api| async move {
            api.transfer(&id, true).await?;
            Ok(None)
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
            Overlay::Devices { devices, state } => match key.code {
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
