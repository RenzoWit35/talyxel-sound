//! OS media controls (media keys, Windows SMTC overlay, macOS Now Playing, MPRIS on Linux).
//!
//! The controls live on a dedicated thread: on Windows they need a window handle and a
//! message pump, which a console app does not have, so we create a hidden window there.

use std::{
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

use crate::event::{AppEvent, MediaCommand};

enum Update {
    Metadata {
        title: String,
        artist: String,
        album: String,
        duration_ms: u32,
    },
    Playback {
        playing: bool,
        position_ms: u32,
    },
}

pub struct MediaKeys {
    tx: mpsc::Sender<Update>,
}

impl MediaKeys {
    pub fn set_metadata(&self, title: &str, artist: &str, album: &str, duration_ms: u32) {
        let _ = self.tx.send(Update::Metadata {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms,
        });
    }

    pub fn set_playback(&self, playing: bool, position_ms: u32) {
        let _ = self.tx.send(Update::Playback {
            playing,
            position_ms,
        });
    }
}

pub fn spawn(app_tx: UnboundedSender<AppEvent>) -> Option<MediaKeys> {
    let (tx, rx) = mpsc::channel::<Update>();
    let (ready_tx, ready_rx) = mpsc::channel::<bool>();

    thread::Builder::new()
        .name("media-keys".into())
        .spawn(move || {
            #[cfg(windows)]
            let window = match win::HiddenWindow::new() {
                Ok(w) => w,
                Err(e) => {
                    warn!("media keys disabled: {e}");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            #[cfg(windows)]
            let hwnd = Some(window.hwnd());
            #[cfg(not(windows))]
            let hwnd = None;

            let config = PlatformConfig {
                display_name: crate::config::APP_TITLE,
                dbus_name: "talyxel_sound",
                hwnd,
            };
            let mut controls = match MediaControls::new(config) {
                Ok(c) => c,
                Err(e) => {
                    warn!("media keys disabled: {e:?}");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            let attached = controls.attach(move |event| {
                let cmd = match event {
                    MediaControlEvent::Play => MediaCommand::Play,
                    MediaControlEvent::Pause => MediaCommand::Pause,
                    MediaControlEvent::Toggle => MediaCommand::Toggle,
                    MediaControlEvent::Next => MediaCommand::Next,
                    MediaControlEvent::Previous => MediaCommand::Previous,
                    _ => return,
                };
                let _ = app_tx.send(AppEvent::Media(cmd));
            });
            if let Err(e) = attached {
                warn!("media keys disabled: {e:?}");
                let _ = ready_tx.send(false);
                return;
            }
            let _ = ready_tx.send(true);

            loop {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(Update::Metadata {
                        title,
                        artist,
                        album,
                        duration_ms,
                    }) => {
                        let _ = controls.set_metadata(MediaMetadata {
                            title: Some(&title),
                            artist: Some(&artist),
                            album: Some(&album),
                            duration: Some(Duration::from_millis(duration_ms as u64)),
                            ..Default::default()
                        });
                    }
                    Ok(Update::Playback {
                        playing,
                        position_ms,
                    }) => {
                        let progress =
                            Some(MediaPosition(Duration::from_millis(position_ms as u64)));
                        let _ = controls.set_playback(if playing {
                            MediaPlayback::Playing { progress }
                        } else {
                            MediaPlayback::Paused { progress }
                        });
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                #[cfg(windows)]
                win::pump_messages();
            }
            let _ = controls.detach();
        })
        .ok()?;

    ready_rx.recv().unwrap_or(false).then_some(MediaKeys { tx })
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
            PeekMessageW, RegisterClassExW, TranslateMessage, WNDCLASSEXW,
        },
    };

    pub struct HiddenWindow(HWND);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }

    impl HiddenWindow {
        pub fn new() -> Result<Self, String> {
            let class = wide("TalyxelSoundMediaWindow");
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                // Registration fails harmlessly if the class already exists.
                RegisterClassExW(&wc);
                let title = wide("");
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    Err(format!(
                        "CreateWindowExW failed: {}",
                        std::io::Error::last_os_error()
                    ))
                } else {
                    Ok(Self(hwnd))
                }
            }
        }

        pub fn hwnd(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    pub fn pump_messages() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
