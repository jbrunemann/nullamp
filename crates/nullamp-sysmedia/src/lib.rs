//! Operating-system media integration.
//!
//! One backend per platform, all through `souvlaki`:
//! - macOS: `MPRemoteCommandCenter` (media keys, Control Center, headphones) and
//!   `MPNowPlayingInfoCenter` (title, artwork, progress).
//! - Linux/BSD: MPRIS over D-Bus (`org.mpris.MediaPlayer2.nullamp`).
//! - Windows: System Media Transport Controls, attached to the main window.
//!
//! Media keys reach the application through these system services, so they work
//! while the window is unfocused and coexist with other players the way the OS
//! arbitrates them (the most recent player owns the keys).
//!
//! - Android: a foreground service with a `MediaSession` (lock screen, headset and Bluetooth buttons, the system
//!   media controls), audio focus and a notification. They are Java classes, so the work is in `android/java/`; this
//!   crate calls them over JNI and takes the presses back through [`on_native_command`] (see `android.rs`).

use std::path::PathBuf;
#[cfg(not(target_os = "android"))]
use std::time::Duration;

use nullamp_core::command::Command;
#[cfg(not(target_os = "android"))]
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

/// What the OS asked the application to do.
#[derive(Clone, Debug, PartialEq)]
pub enum SystemEvent {
    Command(Command),
    /// Bring the window to the front.
    Raise,
    /// A client (MPRIS `OpenUri`) asks to play this URI.
    OpenUri(String),
    Quit,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<f64>,
    /// A local image file for the artwork.
    pub cover: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaybackState {
    Stopped,
    Paused { position: f64 },
    Playing { position: f64 },
}

/// Seek step for "seek forward/backward" requests that carry no amount.
#[cfg(not(target_os = "android"))]
const DEFAULT_SEEK: f64 = 10.0;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{SystemMedia, ask_for_notifications, on_native_command};

#[cfg(not(target_os = "android"))]
pub struct SystemMedia {
    controls: Option<MediaControls>,
    last_metadata: Option<NowPlaying>,
    last_state: Option<PlaybackState>,
}

#[cfg(not(target_os = "android"))]
impl SystemMedia {
    /// Connect to the OS. On failure (no D-Bus session, missing window handle) the
    /// integration is disabled and a warning logged; the player works regardless.
    ///
    /// `hwnd` is required on Windows and ignored elsewhere. `handler` may be called
    /// from any thread.
    pub fn new(handler: impl Fn(SystemEvent) + Send + 'static, hwnd: Option<*mut std::ffi::c_void>) -> Self {
        let config = PlatformConfig { display_name: "Nullamp", dbus_name: "nullamp", hwnd };
        let controls = match MediaControls::new(config) {
            Ok(mut c) => match c.attach(move |ev| {
                if let Some(e) = map_event(ev) {
                    handler(e)
                }
            }) {
                Ok(()) => Some(c),
                Err(e) => {
                    log::warn!("system media controls unavailable: {e:?}");
                    None
                }
            },
            Err(e) => {
                log::warn!("system media controls unavailable: {e:?}");
                None
            }
        };
        SystemMedia { controls, last_metadata: None, last_state: None }
    }

    pub fn is_available(&self) -> bool {
        self.controls.is_some()
    }

    /// Publish track information. Identical repeated updates are skipped.
    pub fn set_metadata(&mut self, now: &NowPlaying) {
        if self.last_metadata.as_ref() == Some(now) {
            return;
        }
        let Some(c) = self.controls.as_mut() else { return };
        let cover_url = now.cover.as_ref().and_then(|p| file_url(p));
        let meta = MediaMetadata {
            title: Some(now.title.as_str()),
            artist: now.artist.as_deref(),
            album: now.album.as_deref(),
            cover_url: cover_url.as_deref(),
            duration: now.duration.filter(|d| d.is_finite() && *d > 0.0).map(Duration::from_secs_f64),
        };
        if let Err(e) = c.set_metadata(meta) {
            log::debug!("set_metadata failed: {e:?}");
        }
        self.last_metadata = Some(now.clone());
        // Some platforms reset the position with new metadata; re-send the state.
        if let Some(state) = self.last_state.take() {
            self.set_playback(state);
        }
    }

    pub fn clear_metadata(&mut self) {
        self.set_metadata(&NowPlaying { title: "Nullamp".into(), ..Default::default() });
    }

    /// Publish playback state. Call on state changes and seeks, not continuously:
    /// the OS extrapolates the position from the last update while playing.
    pub fn set_playback(&mut self, state: PlaybackState) {
        if self.last_state == Some(state) {
            return;
        }
        let Some(c) = self.controls.as_mut() else { return };
        let progress = |p: f64| Some(MediaPosition(Duration::from_secs_f64(p.max(0.0))));
        let playback = match state {
            PlaybackState::Stopped => MediaPlayback::Stopped,
            PlaybackState::Paused { position } => MediaPlayback::Paused { progress: progress(position) },
            PlaybackState::Playing { position } => MediaPlayback::Playing { progress: progress(position) },
        };
        if let Err(e) = c.set_playback(playback) {
            log::debug!("set_playback failed: {e:?}");
        }
        self.last_state = Some(state);
    }
}

#[cfg(not(target_os = "android"))]
fn map_event(ev: MediaControlEvent) -> Option<SystemEvent> {
    let cmd = |c| Some(SystemEvent::Command(c));
    match ev {
        MediaControlEvent::Play => cmd(Command::Play),
        MediaControlEvent::Pause => cmd(Command::Pause),
        MediaControlEvent::Toggle => cmd(Command::PlayPause),
        MediaControlEvent::Next => cmd(Command::Next),
        MediaControlEvent::Previous => cmd(Command::Previous),
        MediaControlEvent::Stop => cmd(Command::Stop),
        MediaControlEvent::Seek(dir) => cmd(Command::SeekBy(signed(dir, DEFAULT_SEEK))),
        MediaControlEvent::SeekBy(dir, d) => cmd(Command::SeekBy(signed(dir, d.as_secs_f64()))),
        MediaControlEvent::SetPosition(MediaPosition(p)) => cmd(Command::SeekTo(p.as_secs_f64())),
        MediaControlEvent::SetVolume(v) => cmd(Command::SetVolume(v.clamp(0.0, 1.0) as f32)),
        MediaControlEvent::Raise => Some(SystemEvent::Raise),
        MediaControlEvent::Quit => Some(SystemEvent::Quit),
        MediaControlEvent::OpenUri(uri) => Some(SystemEvent::OpenUri(uri)),
    }
}

#[cfg(not(target_os = "android"))]
fn signed(dir: SeekDirection, secs: f64) -> f64 {
    match dir {
        SeekDirection::Forward => secs,
        SeekDirection::Backward => -secs,
    }
}

/// What `MediaBridge.nativeCommand` of the Android glue sends: the codes are the constants of `MediaBridge.java`.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn command_from_code(code: i32, arg: f64) -> Option<Command> {
    Some(match code {
        1 => Command::Play,
        2 => Command::Pause,
        3 => Command::PlayPause,
        4 => Command::Next,
        5 => Command::Previous,
        6 => Command::Stop,
        7 => Command::SeekTo(arg.max(0.0)),
        8 => Command::SeekBy(arg),
        _ => return None,
    })
}

/// The `file://` URI of an existing file, percent-encoded.
pub fn file_url(path: &std::path::Path) -> Option<String> {
    let abs = std::fs::canonicalize(path).ok()?;
    let s = abs.to_str()?;
    let mut out = String::from("file://");
    #[cfg(windows)]
    out.push('/');
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(b as char),
            b'\\' => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_events() {
        assert_eq!(map_event(MediaControlEvent::Toggle), Some(SystemEvent::Command(Command::PlayPause)));
        assert_eq!(
            map_event(MediaControlEvent::SeekBy(SeekDirection::Backward, Duration::from_secs(5))),
            Some(SystemEvent::Command(Command::SeekBy(-5.0)))
        );
        assert_eq!(
            map_event(MediaControlEvent::SetPosition(MediaPosition(Duration::from_millis(1500)))),
            Some(SystemEvent::Command(Command::SeekTo(1.5)))
        );
        assert_eq!(map_event(MediaControlEvent::OpenUri("file:///a.mp3".into())), Some(SystemEvent::OpenUri("file:///a.mp3".into())));
    }

    #[test]
    fn android_codes_map_to_commands() {
        assert_eq!(command_from_code(3, 0.0), Some(Command::PlayPause));
        assert_eq!(command_from_code(7, 12.5), Some(Command::SeekTo(12.5)));
        assert_eq!(command_from_code(7, -1.0), Some(Command::SeekTo(0.0)));
        assert_eq!(command_from_code(8, -10.0), Some(Command::SeekBy(-10.0)));
        assert_eq!(command_from_code(99, 0.0), None);
    }

    #[test]
    fn file_urls_are_percent_encoded() {
        let dir = std::env::temp_dir().join("nullamp sysmedia ü");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("cover art.jpg");
        std::fs::write(&f, b"x").unwrap();
        let url = file_url(&f).unwrap();
        assert!(url.starts_with("file:///"));
        assert!(url.contains("nullamp%20sysmedia%20%C3%BC/cover%20art.jpg"), "{url}");
    }
}
