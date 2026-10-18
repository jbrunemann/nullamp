//! Transport commands and their sources.
//!
//! Every way of controlling playback (buttons, keyboard shortcuts, hardware media
//! keys, the OS media overlay) produces a [`Command`]. Commands from all sources go
//! through one queue, so they are handled in order and share one state model.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// Absolute position in seconds.
    SeekTo(f64),
    /// Relative seek in seconds.
    SeekBy(f64),
    /// 0.0 ..= 1.0
    SetVolume(f32),
    ToggleMute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSource {
    /// Mouse interaction with the window.
    Ui,
    /// A shortcut pressed while the window is focused.
    Keyboard,
    /// OS media integration: media keys, Now Playing overlay, MPRIS, SMTC.
    System,
}

/// Drops a media command that arrives twice through different integration paths.
///
/// Some desktop environments deliver a hardware media key both as a key event to the
/// focused window and through the system media interface. A repeat of the same command
/// from a *different* source inside the window is treated as the same key press.
/// Repeats from the same source are always accepted (the user pressed twice).
#[derive(Debug)]
pub struct Deduplicator {
    window: Duration,
    last: Option<(Command, CommandSource, Instant)>,
}

impl Deduplicator {
    pub fn new(window: Duration) -> Self {
        Deduplicator { window, last: None }
    }

    pub fn accept(&mut self, command: Command, source: CommandSource, now: Instant) -> bool {
        let dedupable = matches!(
            command,
            Command::PlayPause | Command::Play | Command::Pause | Command::Stop | Command::Next | Command::Previous
        );
        if dedupable
            && let Some((last_cmd, last_src, at)) = self.last
            && last_cmd == command
            && last_src != source
            && now.saturating_duration_since(at) < self.window
        {
            return false;
        }
        self.last = Some((command, source, now));
        true
    }
}

impl Default for Deduplicator {
    fn default() -> Self {
        Deduplicator::new(Duration::from_millis(150))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_cross_source_duplicates_only() {
        let mut d = Deduplicator::default();
        let t0 = Instant::now();
        assert!(d.accept(Command::PlayPause, CommandSource::System, t0));
        assert!(!d.accept(Command::PlayPause, CommandSource::Keyboard, t0 + Duration::from_millis(20)));
        assert!(d.accept(Command::PlayPause, CommandSource::Keyboard, t0 + Duration::from_millis(400)));
        assert!(d.accept(Command::PlayPause, CommandSource::Keyboard, t0 + Duration::from_millis(410)), "same source");
        assert!(d.accept(Command::Next, CommandSource::System, t0 + Duration::from_millis(420)), "different command");
    }

    #[test]
    fn seeks_and_volume_are_never_dropped() {
        let mut d = Deduplicator::default();
        let t0 = Instant::now();
        assert!(d.accept(Command::SeekBy(5.0), CommandSource::System, t0));
        assert!(d.accept(Command::SeekBy(5.0), CommandSource::Keyboard, t0));
    }
}
