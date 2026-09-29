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
- **Your library without a developer app**: playlists, Liked Songs, search, Recently Played and
  the device list come through the same Spotify connection the official apps use, so they
  aren't affected by the Web API limits described below.
- **Live updates**: Spotify pushes changes made on other devices (track, pause, seek, volume,
  shuffle/repeat) to the app as they happen. Playlist and Liked Songs edits are picked up
  automatically.
- **Spectrum visualizer**: a real FFT of the audio playing locally.
- **Media keys** and the OS media overlay (Windows SMTC, macOS Now Playing, MPRIS on Linux).
- **Secure login**: a one-time browser login. The refresh token is kept in the OS credential
  store (Windows Credential Manager / macOS Keychain / Secret Service) and never in plaintext.
- **Self-update**: checks GitHub Releases on startup. Press `U` or run `talyxel update`.

## Requirements

- **Spotify Premium**. librespot streaming requires it. Without Premium the app can still work
  as a remote control for other devices, but only with your own `client_id` (see below).

## Install

The installers download the prebuilt app from the latest
[GitHub release](https://github.com/RenzoWit35/talyxel-sound/releases). You don't need Rust or
any build tools.

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/RenzoWit35/talyxel-sound/master/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/RenzoWit35/talyxel-sound/master/install.ps1 | iex
```

Then run `talyxel`. The first run opens the browser to link Spotify.

Prebuilt binaries are available for Windows x64, macOS (Apple Silicon and Intel) and Linux x64
(glibc 2.35 or newer, e.g. Ubuntu 22.04+ or Debian 12+). The only thing Linux needs is the ALSA
sound library (`libasound2`), which desktop distros already include. To install by hand,
download the archive for your platform from the releases page and put `talyxel` on your `PATH`.
On macOS, a file downloaded in a browser needs `xattr -d com.apple.quarantine talyxel` before it
will run.

## Build from source

You need Rust 1.85 or newer. On Windows you also need the MSVC C++ Build Tools. On Linux you
need `libasound2-dev pkg-config`.

```bash
cargo build --release
./target/release/talyxel
```

`cargo test` runs the offline tests. To check the library, search and Spotify Connect against
your own account (log in with `talyxel` first), run
`cargo test live_ -- --ignored --nocapture --test-threads=1`. It plays a song on this computer
for a few seconds. Set `TALYXEL_LIVE_TARGET` to the name of one of your devices to also test
moving playback there and back.

## Usage

| Command            | What it does                                        |
|--------------------|-----------------------------------------------------|
| `talyxel`          | Start the player                                    |
| `talyxel login`    | Link or re-link your Spotify account                |
| `talyxel logout`   | Remove the stored credentials from the OS keyring   |
| `talyxel update`   | Install the newest GitHub release                   |
| `talyxel demo`     | Preview the UI with mock data (no account needed)   |
| `talyxel paths`    | Show where the config and log file live             |

## Keys

| Key            | Action                                  |
|----------------|-----------------------------------------|
| ↑/↓, j/k       | Move selection                          |
| Tab, h/l       | Switch between sidebar and list         |
| Enter          | Open item / play track                  |
| Space          | Play / pause                            |
| n / p          | Next / previous track                   |
| ← / →          | Seek −5 s / +5 s                        |
| + / −          | Volume                                  |
| s / r          | Shuffle / repeat (off→all→one)          |
| /              | Search                                  |
| d              | Devices: Enter plays there, r refreshes |
| U              | Install available update                |
| ?              | Help                                    |
| q, Ctrl-C      | Quit                                    |

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
update_repo_owner = "RenzoWit35"
update_repo_name = "talyxel-sound"
# Optional: your own Spotify developer app for Web API requests (see "Rate limits" below)
# client_id = "..."
# redirect_uri = "http://127.0.0.1:8898/login"   # the default
```

Logs are written to `talyxel.log` in the data directory. Set `TALYXEL_LOG=debug` for more detail.

## Rate limits: using your own Spotify app

Playback has to log in as Spotify's desktop client. Since December 2025 Spotify answers Web API
requests made with that client's login with `429 Too Many Requests`, often asking for a wait of
a whole day. Talyxel Sound therefore doesn't use the Web API while local playback is running:
playlists, Liked Songs, search, Recently Played, the device list and remote control go through
the playback connection instead, like in Spotify's own apps. With Premium you don't need to set
anything up.

When local playback isn't available (for example without Premium), the app falls back to the
Web API. Spotify limits Web API requests per *app*, so the shared desktop client will mostly be
refused and you'll see `Spotify rate limit: try again in …`. Your own free developer app gets a
limit of its own:

1. Open the [Spotify developer dashboard](https://developer.spotify.com/dashboard) and click
   **Create app**. Any name and description will do.
2. Add the redirect URI `http://127.0.0.1:8898/login`, tick **Web API**, and save.
3. Copy the app's **Client ID** and add it to `config.toml` (run `talyxel paths` to find it):
   `client_id = "your-client-id"`.
4. Start `talyxel`. The browser asks once to allow your app to use your account.

Spotify's rules for these development-mode apps: they only work while the account that created
the app has Premium, and they return at most 10 search results per request, so a search makes
three requests to show 30 results. They also can't read the tracks of playlists you don't own or collaborate on, such as
Discover Weekly, so Talyxel Sound loads those through the desktop client.

## Publishing updates

Updates are fetched from the GitHub repo
[RenzoWit35/talyxel-sound](https://github.com/RenzoWit35/talyxel-sound). If you fork it, change
`update_repo_owner` / `update_repo_name` in `src/config.rs`, `repository` in `Cargo.toml`, and the
repo name and URLs in `install.sh`, `install.ps1` and the install commands above.

1. Create the `talyxel-sound` repo on GitHub and push this code to it. The repo must be public:
   the installers and the updater download without logging in to GitHub.
2. Bump `version` in `Cargo.toml`, commit, and push a tag for that exact version with a
   lowercase `v`: `git tag v0.3.0 && git push --tags`. A release created on GitHub's website
   needs the same tag. The build stops if the tag and `version` don't match, because the
   updater would otherwise keep offering the same release.
3. `.github/workflows/release.yml` builds Windows, macOS and Linux binaries and attaches them to
   the release. Running copies see the update on their next start. The same workflow also runs
   (without publishing) on pull requests that change the build setup.

[librespot]: https://github.com/librespot-org/librespot
