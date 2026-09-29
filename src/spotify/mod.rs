pub mod api;
pub mod internal;
pub mod observer;
pub mod player;
pub mod sink;

/// Simplified track used throughout the UI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub year: Option<String>,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub snapshot_id: String,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    pub is_active: bool,
    pub volume: Option<u8>,
    /// What kind of device it is ("Computer", "Phone", "Speaker", …); may be empty.
    pub kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

impl Repeat {
    pub fn cycle(self) -> Self {
        match self {
            Repeat::Off => Repeat::Context,
            Repeat::Context => Repeat::Track,
            Repeat::Track => Repeat::Off,
        }
    }
}

/// Playback state as reported by the Web API (any device).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaybackState {
    pub device: Option<Device>,
    pub track: Option<Track>,
    pub is_playing: bool,
    pub progress_ms: u32,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub context_uri: Option<String>,
}

pub fn format_ms(ms: u32) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}
