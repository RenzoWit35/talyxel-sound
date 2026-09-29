# Talyxel Sound

A terminal Spotify client written in Rust. Talyxel Sound registers itself as a real
**Spotify Connect** device (via [librespot]), so audio plays locally without the official
client, and it shows up in the device list on your phone and desktop.

```
┌──────────────────── Talyxel Sound - TUI v0.1.0 ─────────────────────┐
│     .-~~~~~~~~~~~~-.      │  Now Playing                            │
│   .'  ____________  '.    │  Pink Floyd - Wish You Were Here        │
│  /   |____    ____|   \   │  Wish You Were Here (1975)              │
│ |         |  |         |  │─────────────────────────────────────────│
│  \        |  |        /   │  █ █ ▇ ▅ ▃ ▁   ▂ ▅ ▇ █ █ ▇ ▆ ▅ ▃ ▂       │
│   '.      |__|      .'    │  [>==========--------------] 2:54 / 5:59 │
│ Browse                    │  [Playing]  [<<] [||] [>>]   [Vol : 75%] │
│ Playlists                 │  Discover Weekly (5)                    │
│   Discover Weekly         │  ♪ Wish You Were Here — Pink Floyd  5:59 │
└─────────────────────────────────────────────────────────────────────┘
```

## Features

- **Spotify Connect device**: play locally, or control and transfer playback to any device (`d`).
- **Live updates**: changes made on other devices (track, pause, seek, volume, shuffle/repeat)
  appear within about a second. Playlist and Liked Songs edits are picked up automatically.
- **Spectrum visualizer**: a real FFT of the audio playing locally.
- **Media keys** and the OS media overlay (Windows SMTC, macOS Now Playing, MPRIS on Linux).
- **Secure login**: a one-time browser login. The refresh token is kept in the OS credential
  store (Windows Credential Manager / macOS Keychain / Secret Service) and never in plaintext.
- **Self-update**: checks GitHub Releases on startup. Press `U` or run `talyxel update`.

## Requirements

- **Spotify Premium**. librespot streaming requires it. Without Premium the app still works as
  a remote control for other devices.
- To build from source: Rust (stable). On Windows you also need the MSVC C++ Build Tools. On
  Linux you need `libasound2-dev libssl-dev pkg-config`.

## Install / run

```bash
cargo build --release
./target/release/talyxel          # first run opens the browser to link Spotify
```

| Command            | What it does                                        |
|--------------------|-----------------------------------------------------|
| `talyxel`          | Start the player                                    |
| `talyxel login`    | Link or re-link your Spotify account                |
| `talyxel logout`   | Remove the stored credentials from the OS keyring   |
| `talyxel update`   | Install the newest GitHub release                   |
| `talyxel demo`     | Preview the UI with mock data (no account needed)   |
| `talyxel paths`    | Show where the config and log file live             |

## Keys

| Key            | Action                           |
|----------------|----------------------------------|
| ↑/↓, j/k       | Move selection                   |
| Tab, h/l       | Switch between sidebar and list  |
| Enter          | Open item / play track           |
| Space          | Play / pause                     |
| n / p          | Next / previous track            |
| ← / →          | Seek −5 s / +5 s                 |
| + / −          | Volume                           |
| s / r          | Shuffle / repeat (off→all→one)   |
| /              | Search                           |
| d              | Devices (transfer playback)      |
| U              | Install available update         |
| ?              | Help                             |
| q, Ctrl-C      | Quit                             |

## Configuration

`config.toml` is created on first run. Run `talyxel paths` to find it.

```toml
device_name = "Talyxel Sound"
bitrate = 320                  # 96, 160 or 320
initial_volume = 75
remote_poll_active_ms = 1000   # poll rate while another device plays
remote_poll_idle_ms = 5000
library_refresh_secs = 60      # how often playlists are checked for changes
check_updates = true
update_repo_owner = "your-github-user"
update_repo_name = "talyxel-sound"
# Optional: use your own Spotify developer app (both required)
# client_id = "..."
# redirect_uri = "http://127.0.0.1:8898/login"
```

Logs are written to `talyxel.log` in the data directory. Set `TALYXEL_LOG=debug` for more detail.

## Publishing updates

1. Set `update_repo_owner` / `update_repo_name` in the defaults in `src/config.rs` (and
   `repository` in `Cargo.toml`) to your GitHub repo.
2. Bump `version` in `Cargo.toml`, commit, and push a tag: `git tag v0.2.0 && git push --tags`.
3. `.github/workflows/release.yml` builds Windows, macOS and Linux binaries and attaches them to
   the release. Running copies see the update on their next start.

[librespot]: https://github.com/librespot-org/librespot
