//! Offline preview (`talyxel demo`) with mock data, used to try the UI without an account.

use std::sync::{Arc, atomic::AtomicBool};

use tokio::sync::{Notify, mpsc};

use crate::{
    app::{App, Focus},
    config::Config,
    event::{AppEvent, View},
    spotify::{Device, Playlist, Track, api::Api, sink::SampleTap},
};

fn track(name: &str, artists: &str, album: &str, year: &str, secs: u32) -> Track {
    Track {
        uri: format!("spotify:track:{}", name.to_lowercase().replace(' ', "")),
        name: name.into(),
        artists: artists.into(),
        album: album.into(),
        year: Some(year.into()),
        duration_ms: secs * 1000,
    }
}

pub fn devices() -> Vec<Device> {
    vec![
        Device {
            id: Some("1".into()),
            name: "Talyxel Sound".into(),
            is_active: true,
            volume: Some(75),
        },
        Device {
            id: Some("2".into()),
            name: "Phone".into(),
            is_active: false,
            volume: Some(40),
        },
        Device {
            id: Some("3".into()),
            name: "Living Room Speaker".into(),
            is_active: false,
            volume: Some(55),
        },
    ]
}

pub fn app(tx: mpsc::UnboundedSender<AppEvent>) -> App {
    let dummy = rspotify::Token {
        access_token: "demo".into(),
        ..rspotify::Token::default()
    };
    let api = Api::new(dummy, "demo", |_| {});
    let mut app = App::new(
        Config::default(),
        api,
        None,
        SampleTap::default(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Notify::new()),
        tx,
        None,
    );
    app.demo = true;
    app.playlists = ["Starred", "Discover Weekly", "90s Rock", "Ambient"]
        .iter()
        .enumerate()
        .map(|(i, n)| Playlist {
            id: i.to_string(),
            uri: format!("spotify:playlist:{i}"),
            name: n.to_string(),
            snapshot_id: "0".into(),
            total: 5,
        })
        .collect();
    app.tracks = vec![
        track(
            "Shine On You Crazy Diamond",
            "Pink Floyd",
            "Wish You Were Here",
            "1975",
            811,
        ),
        track(
            "Wish You Were Here",
            "Pink Floyd",
            "Wish You Were Here",
            "1975",
            359,
        ),
        track(
            "Welcome to the Machine",
            "Pink Floyd",
            "Wish You Were Here",
            "1975",
            451,
        ),
        track(
            "Have a Cigar",
            "Pink Floyd",
            "Wish You Were Here",
            "1975",
            308,
        ),
        track(
            "Time",
            "Pink Floyd",
            "The Dark Side of the Moon",
            "1973",
            413,
        ),
    ];
    app.view = View::Playlist {
        id: "1".into(),
        uri: "spotify:playlist:1".into(),
        name: "Discover Weekly".into(),
    };
    app.track_state.select(Some(1));
    app.sidebar.select(Some(5));
    app.focus = Focus::Tracks;
    app.now.track = Some(app.tracks[1].clone());
    app.now.is_playing = true;
    app.now.position_ms = 174_000;
    app.now.position_at = Some(std::time::Instant::now());
    app.now.device_name = Some("Talyxel Sound".into());
    app.now.volume = 75;
    app.update_available = Some("0.2.0".into());
    app
}
