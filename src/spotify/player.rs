//! Local playback: a librespot session registered as a Spotify Connect device.

use anyhow::{Context, Result, anyhow};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::{Session, SessionConfig, authentication::Credentials, config::DeviceType};
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::{self, MixerConfig},
    player::{Player, PlayerEventChannel},
};
use tracing::info;

use super::sink::{SampleTap, TapSink};
use crate::config::{self, Config};

pub struct LocalPlayer {
    spirc: Spirc,
    pub session: Session,
}

impl LocalPlayer {
    /// Connects to Spotify and registers the Connect device. Returns the player event stream.
    pub async fn start(
        cfg: &Config,
        access_token: &str,
        tap: SampleTap,
    ) -> Result<(Self, PlayerEventChannel)> {
        let session_config = SessionConfig {
            device_id: config::device_id()?,
            ..SessionConfig::default()
        };
        let session = Session::new(session_config, None);

        let mixer_fn = mixer::find(None).ok_or_else(|| anyhow!("no audio mixer available"))?;
        let mixer = mixer_fn(MixerConfig::default()).context("opening mixer")?;

        let backend = audio_backend::find(None).ok_or_else(|| anyhow!("no audio backend"))?;
        let player_config = PlayerConfig {
            bitrate: match cfg.bitrate {
                96 => Bitrate::Bitrate96,
                160 => Bitrate::Bitrate160,
                _ => Bitrate::Bitrate320,
            },
            ..PlayerConfig::default()
        };
        let player = Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || Box::new(TapSink::new(backend(None, AudioFormat::default()), tap)),
        );
        let events = player.get_player_event_channel();

        let connect_config = ConnectConfig {
            name: cfg.device_name.clone(),
            device_type: DeviceType::Computer,
            initial_volume: percent_to_volume(cfg.initial_volume),
            ..ConnectConfig::default()
        };
        let (spirc, spirc_task) = Spirc::new(
            connect_config,
            session.clone(),
            Credentials::with_access_token(access_token),
            player,
            mixer.clone(),
        )
        .await
        .context("connecting to Spotify (a Premium account is required)")?;
        tokio::spawn(spirc_task);
        info!("Spotify Connect device '{}' is online", cfg.device_name);

        Ok((Self { spirc, session }, events))
    }

    /// Plays a context (playlist, Liked Songs, search results) from `track_uri` on this device.
    pub fn load_context(&self, context_uri: &str, track_uri: &str) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            playing_track: Some(PlayingTrack::Uri(track_uri.to_string())),
            ..LoadRequestOptions::default()
        };
        self.load(LoadRequest::from_context_uri(
            context_uri.to_string(),
            options,
        ))
    }

    /// Plays a list of tracks on this device, starting at `index`.
    pub fn load_tracks(&self, tracks: &[String], index: usize) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            playing_track: Some(PlayingTrack::Index(index as u32)),
            ..LoadRequestOptions::default()
        };
        self.load(LoadRequest::from_tracks(tracks.to_vec(), options))
    }

    fn load(&self, request: LoadRequest) -> Result<()> {
        self.spirc.activate()?;
        self.spirc.load(request)?;
        Ok(())
    }

    pub fn play_pause(&self) -> Result<()> {
        Ok(self.spirc.play_pause()?)
    }
    pub fn next(&self) -> Result<()> {
        Ok(self.spirc.next()?)
    }
    pub fn prev(&self) -> Result<()> {
        Ok(self.spirc.prev()?)
    }
    pub fn seek(&self, position_ms: u32) -> Result<()> {
        Ok(self.spirc.set_position_ms(position_ms)?)
    }
    pub fn set_volume_percent(&self, percent: u8) -> Result<()> {
        Ok(self.spirc.set_volume(percent_to_volume(percent))?)
    }
    pub fn shuffle(&self, on: bool) -> Result<()> {
        Ok(self.spirc.shuffle(on)?)
    }
    pub fn repeat(&self, context: bool, track: bool) -> Result<()> {
        self.spirc.repeat(context)?;
        Ok(self.spirc.repeat_track(track)?)
    }
    /// Take over playback from whichever device is currently active.
    pub fn transfer_here(&self) -> Result<()> {
        Ok(self.spirc.transfer(None)?)
    }

    pub fn shutdown(&self) {
        let _ = self.spirc.shutdown();
        self.session.shutdown();
    }
}

pub fn percent_to_volume(percent: u8) -> u16 {
    ((percent.min(100) as u32 * u16::MAX as u32) / 100) as u16
}

pub fn volume_to_percent(volume: u16) -> u8 {
    ((volume as u32 * 100 + u16::MAX as u32 / 2) / u16::MAX as u32) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_conversion_round_trips() {
        for p in 0..=100u8 {
            assert_eq!(volume_to_percent(percent_to_volume(p)), p);
        }
        assert_eq!(percent_to_volume(200), u16::MAX);
    }
}
