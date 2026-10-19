//! Core data models and pure logic for Nullamp.
//!
//! Nothing in this crate touches audio devices, the network or the UI. Everything that
//! can be tested without hardware lives here: playlist editing, play order, setlist
//! modelling and validation, and the text formats (M3U, cue sheets, pasted tracklists).

pub mod command;
pub mod cue;
pub mod ids;
pub mod key;
pub mod m3u;
pub mod playlist;
pub mod queue;
pub mod setlist;
pub mod text;
pub mod time;
pub mod track;
pub mod tracklist_text;

pub use ids::EntryId;
pub use key::{Key, KeyNotation};
pub use playlist::{EntryStatus, Playlist, PlaylistEntry};
pub use queue::{Navigator, RepeatMode};
pub use setlist::{Setlist, SetlistEntry};
pub use track::TrackInfo;
