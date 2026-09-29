//! Live update detection for changes made outside this app.
//!
//! * Playback on other devices: follows the Connect cluster updates Spotify pushes to the
//!   Connect observer, re-reading the cluster now and then (or polling the Web API when there
//!   is no playback session), and only emits when something meaningful changed (track, play state, device,
//!   shuffle/repeat/volume, or a position jump).
//! * Library: periodically compares playlist snapshot ids and the Liked Songs head/count.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::{Notify, mpsc::UnboundedSender};
use tracing::warn;

use crate::{
    config::Config,
    event::AppEvent,
    spotify::{PlaybackState, Playlist, api::Api, observer::ClusterUpdates},
};

/// Allowed drift between the interpolated and the reported position before we resync.
const POSITION_TOLERANCE_MS: i64 = 1500;
/// Retry delay while the playlists have never loaded.
const FIRST_LOAD_RETRY: Duration = Duration::from_secs(10);
/// Retry delay after a failed playback read while the session is still connecting.
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// How often playback is re-read while Spotify pushes Connect changes anyway.
const PUSHED_POLL: Duration = Duration::from_secs(60);

pub fn spawn_remote_poller(
    api: Api,
    cfg: &Config,
    local_active: Arc<AtomicBool>,
    wake: Arc<Notify>,
    tx: UnboundedSender<AppEvent>,
    mut pushes: Option<ClusterUpdates>,
) {
    let active = Duration::from_millis(cfg.remote_poll_active_ms.max(250));
    let idle = Duration::from_millis(cfg.remote_poll_idle_ms.max(active.as_millis() as u64));
    let pushed = pushes.is_some() && api.internal().is_some();
    tokio::spawn(async move {
        let mut last: Option<PlaybackState> = None;
        let mut last_at = Instant::now();
        let mut first = true;
        let mut delay = Duration::ZERO;
        loop {
            let result: Result<Option<PlaybackState>> = tokio::select! {
                _ = tokio::time::sleep(delay) => api.playback().await,
                _ = wake.notified() => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    api.playback().await
                }
                Some(update) = next_update(&mut pushes) => match api.internal() {
                    Some(internal) => Ok(internal.playback_from_update(&update).await),
                    None => continue,
                },
            };
            match result {
                Ok(state) => {
                    let elapsed = last_at.elapsed().as_millis() as i64;
                    if (first || significant_change(last.as_ref(), state.as_ref(), elapsed))
                        && tx.send(AppEvent::Remote(state.clone())).is_err()
                    {
                        return;
                    }
                    first = false;
                    let local = local_active.load(Ordering::Relaxed);
                    delay = poll_delay(pushed, state.as_ref(), local, active, idle);
                    last = state;
                    last_at = Instant::now();
                }
                Err(e) => {
                    warn!("reading playback failed: {e:#}");
                    // The Web API client holds back requests until Spotify's wait is over.
                    delay = if pushed {
                        RETRY_DELAY
                    } else {
                        idle.max(api.rate_limited_for())
                    };
                }
            }
        }
    });
}

/// The next cluster update Spotify pushed, skipping ones that can't be read. Never
/// finishes when there is no playback session to receive them.
async fn next_update(
    pushes: &mut Option<ClusterUpdates>,
) -> Option<librespot_protocol::connect::ClusterUpdate> {
    let Some(stream) = pushes else {
        return std::future::pending().await;
    };
    loop {
        match stream.next().await? {
            Ok(update) => return Some(update),
            Err(e) => warn!("unreadable Connect update: {e}"),
        }
    }
}

/// Wait before the next playback poll.
fn poll_delay(
    pushed: bool,
    state: Option<&PlaybackState>,
    local_active: bool,
    active: Duration,
    idle: Duration,
) -> Duration {
    if pushed {
        PUSHED_POLL
    } else if state.is_some_and(|s| s.is_playing) && !local_active {
        active
    } else {
        idle
    }
}

/// True if `next` differs from `prev` in a way the UI can't predict on its own.
pub fn significant_change(
    prev: Option<&PlaybackState>,
    next: Option<&PlaybackState>,
    elapsed_ms: i64,
) -> bool {
    match (prev, next) {
        (None, None) => false,
        (Some(a), Some(b)) => {
            let same_meta = a.track == b.track
                && a.is_playing == b.is_playing
                && a.shuffle == b.shuffle
                && a.repeat == b.repeat
                && a.device == b.device
                && a.context_uri == b.context_uri;
            if !same_meta {
                return true;
            }
            let expected = a.progress_ms as i64 + if a.is_playing { elapsed_ms } else { 0 };
            (b.progress_ms as i64 - expected).abs() > POSITION_TOLERANCE_MS
        }
        _ => true,
    }
}

pub fn spawn_library_watcher(api: Api, cfg: &Config, tx: UnboundedSender<AppEvent>) {
    let every = Duration::from_secs(cfg.library_refresh_secs.max(10));
    tokio::spawn(async move {
        let mut known: Option<Vec<Playlist>> = None;
        let mut liked: Option<(u32, Option<String>)> = None;
        loop {
            match api.playlists().await {
                Ok(playlists) => {
                    let event = match &known {
                        None => Some(AppEvent::Playlists {
                            playlists: playlists.clone(),
                            changed: Vec::new(),
                            initial: true,
                        }),
                        Some(old) => {
                            let changed = diff_playlists(old, &playlists);
                            let reordered = !same_order(old, &playlists);
                            (!changed.is_empty() || reordered).then(|| AppEvent::Playlists {
                                playlists: playlists.clone(),
                                changed,
                                initial: false,
                            })
                        }
                    };
                    if let Some(ev) = event
                        && tx.send(ev).is_err()
                    {
                        return;
                    }
                    known = Some(playlists);
                }
                Err(e) => {
                    warn!("playlist refresh failed: {e:#}");
                    if known.is_none() {
                        let _ = tx.send(AppEvent::PlaylistsFailed(format!("{e:#}")));
                    }
                }
            }

            if let Ok(head) = api.liked_total().await {
                if liked.as_ref().is_some_and(|prev| *prev != head) {
                    let _ = tx.send(AppEvent::LikedChanged);
                }
                liked = Some(head);
            }

            // Until the playlists have loaded once, try again soon.
            let wait = if known.is_some() {
                every
            } else {
                every.min(FIRST_LOAD_RETRY)
            };
            tokio::time::sleep(wait).await;
        }
    });
}

/// Ids of playlists that were added, removed, renamed or whose contents changed.
pub fn diff_playlists(old: &[Playlist], new: &[Playlist]) -> Vec<String> {
    let old_map: HashMap<&str, &Playlist> = old.iter().map(|p| (p.id.as_str(), p)).collect();
    let new_map: HashMap<&str, &Playlist> = new.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut changed: Vec<String> = new
        .iter()
        .filter(|p| {
            old_map
                .get(p.id.as_str())
                .is_none_or(|o| o.snapshot_id != p.snapshot_id || o.name != p.name)
        })
        .map(|p| p.id.clone())
        .collect();
    changed.extend(
        old.iter()
            .filter(|p| !new_map.contains_key(p.id.as_str()))
            .map(|p| p.id.clone()),
    );
    changed
}

fn same_order(old: &[Playlist], new: &[Playlist]) -> bool {
    old.len() == new.len() && old.iter().zip(new).all(|(a, b)| a.id == b.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::Track;

    fn pl(id: &str, snap: &str) -> Playlist {
        Playlist {
            id: id.into(),
            uri: format!("spotify:playlist:{id}"),
            name: id.to_uppercase(),
            snapshot_id: snap.into(),
            total: 1,
        }
    }

    fn state(track: &str, playing: bool, pos: u32) -> PlaybackState {
        PlaybackState {
            track: Some(Track {
                uri: track.into(),
                ..Track::default()
            }),
            is_playing: playing,
            progress_ms: pos,
            ..PlaybackState::default()
        }
    }

    #[test]
    fn polls_slowly_when_spotify_pushes_changes() {
        let (active, idle) = (Duration::from_secs(1), Duration::from_secs(5));
        let playing_elsewhere = Some(state("t1", true, 0));
        // Web API only: poll fast while another device plays.
        assert_eq!(
            poll_delay(false, playing_elsewhere.as_ref(), false, active, idle),
            active
        );
        assert_eq!(poll_delay(false, None, false, active, idle), idle);
        assert_eq!(
            poll_delay(false, playing_elsewhere.as_ref(), true, active, idle),
            idle
        );
        // With Connect pushes, polling is only a safety net.
        assert_eq!(
            poll_delay(true, playing_elsewhere.as_ref(), false, active, idle),
            PUSHED_POLL
        );
    }

    #[test]
    fn detects_playlist_changes() {
        let old = vec![pl("a", "1"), pl("b", "1"), pl("c", "1")];
        let new = vec![pl("a", "1"), pl("b", "2"), pl("d", "1")];
        let mut changed = diff_playlists(&old, &new);
        changed.sort();
        assert_eq!(changed, vec!["b", "c", "d"]);
        assert!(diff_playlists(&old, &old).is_empty());
    }

    #[test]
    fn position_progress_is_not_a_change() {
        let a = state("t1", true, 10_000);
        let b = state("t1", true, 11_000);
        assert!(!significant_change(Some(&a), Some(&b), 1000));
    }

    #[test]
    fn seek_track_and_pause_are_changes() {
        let a = state("t1", true, 10_000);
        assert!(significant_change(
            Some(&a),
            Some(&state("t1", true, 60_000)),
            1000
        ));
        assert!(significant_change(
            Some(&a),
            Some(&state("t2", true, 0)),
            1000
        ));
        assert!(significant_change(
            Some(&a),
            Some(&state("t1", false, 11_000)),
            1000
        ));
        assert!(significant_change(Some(&a), None, 1000));
        assert!(!significant_change(None, None, 1000));
    }
}
