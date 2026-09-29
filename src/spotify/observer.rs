//! A second, hidden Spotify connection that follows the account's Connect cluster.
//!
//! Spotify pushes every Connect change (devices coming and going, playback on any of them)
//! to the members of the cluster, and a member can read the device list whenever it wants.
//! Membership needs a dealer connection, but it can't be the playback device's own: Spotify
//! would then echo that device's state updates back to it, and librespot answers every update
//! with another one. That loop repeats every few hundred milliseconds until Spotify refuses
//! further updates (429). So the observer has a connection of its own and registers on it as
//! a hidden device that can't play, like Spotify's web player does.

use anyhow::{Context, Result};
use futures::StreamExt;
use librespot_core::{
    Error, Session, SessionConfig,
    authentication::Credentials,
    dealer::{manager::BoxedStreamResult, protocol::Message},
};
use librespot_protocol::connect::ClusterUpdate;
use tracing::{debug, warn};

/// Connect cluster updates (devices and playback of the whole account) pushed by Spotify.
pub type ClusterUpdates = BoxedStreamResult<ClusterUpdate>;

/// Connects the observer. Returns its session, for Connect requests, and the updates
/// Spotify pushes to it once it has registered.
pub async fn start(access_token: &str) -> Result<(Session, ClusterUpdates)> {
    let session = Session::new(SessionConfig::default(), None);
    // Listeners have to be in place before the dealer connection starts.
    let updates = session
        .dealer()
        .listen_for("hm://connect-state/v1/cluster", Message::from_raw)?;
    let mut connection_ids =
        session
            .dealer()
            .listen_for("hm://pusher/v1/connections/", |msg: Message| {
                msg.headers
                    .get("Spotify-Connection-Id")
                    .cloned()
                    .ok_or_else(|| Error::invalid_argument("no Spotify-Connection-Id header"))
            })?;
    session
        .connect(Credentials::with_access_token(access_token), false)
        .await
        .context("connecting the Spotify Connect observer")?;
    session
        .dealer()
        .start()
        .await
        .context("starting the Spotify Connect observer")?;
    let s = session.clone();
    tokio::spawn(async move {
        while let Some(id) = connection_ids.next().await {
            match id {
                Ok(id) => {
                    debug!("Spotify Connect observer connected");
                    s.set_connection_id(&id);
                }
                Err(e) => warn!("unreadable Spotify Connect connection id: {e}"),
            }
        }
    });
    Ok((session, updates))
}
