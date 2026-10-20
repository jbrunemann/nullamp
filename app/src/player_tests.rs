use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;

use nullamp_audio::{EngineEvent, LoadError, StreamInfo};
use nullamp_core::command::{Command, CommandSource};
use nullamp_core::{EntryId, EntryStatus, Playlist, RepeatMode};

use super::{Engine, Note, Player, Status};
use crate::store::PlaylistDoc;

#[derive(Clone, Debug, PartialEq)]
enum Call {
    Load { token: u64, path: String, start_at: f64, play: bool },
    Preload(Option<(u64, String)>),
    Paused(bool),
    Stop,
    Seek(f64),
}

#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<Call>>,
    pos: Cell<(u64, f64)>,
}

impl Engine for Fake {
    fn load(&self, token: u64, path: PathBuf, start_at: f64, play: bool) {
        self.calls.borrow_mut().push(Call::Load { token, path: path.display().to_string(), start_at, play });
    }
    fn preload(&self, next: Option<(u64, PathBuf)>) {
        self.calls.borrow_mut().push(Call::Preload(next.map(|(t, p)| (t, p.display().to_string()))));
    }
    fn set_paused(&self, paused: bool) {
        self.calls.borrow_mut().push(Call::Paused(paused));
    }
    fn stop(&self) {
        self.calls.borrow_mut().push(Call::Stop);
    }
    fn seek(&self, secs: f64) {
        self.calls.borrow_mut().push(Call::Seek(secs));
    }
    fn set_gain(&self, _: f32) {}
    fn position(&self) -> (u64, f64) {
        self.pos.get()
    }
}

fn info() -> StreamInfo {
    StreamInfo { duration: Some(200.0), sample_rate: 44100, channels: 2, codec: "MP3".into(), bitrate: Some(320) }
}

fn player(n: usize) -> (Player<Fake>, Vec<EntryId>) {
    let mut p = Playlist::new("t");
    let ids = p.add_paths((0..n).map(|i| PathBuf::from(format!("{i}.mp3"))));
    let player = Player::new(Fake::default(), vec![PlaylistDoc { key: "t".into(), playlist: p }], 1);
    (player, ids)
}

impl Player<Fake> {
    fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.engine.calls.borrow_mut())
    }
    fn cur_token(&self) -> u64 {
        self.current.as_ref().unwrap().token
    }
    fn loaded(&mut self) {
        let token = self.cur_token();
        self.on_engine_event(EngineEvent::Loaded { token, info: info() });
    }
    fn current_path(&self) -> String {
        self.current.as_ref().unwrap().path.display().to_string()
    }
}

fn last_load(calls: &[Call]) -> Option<&Call> {
    calls.iter().rev().find(|c| matches!(c, Call::Load { .. }))
}

#[test]
fn play_load_and_preload_next() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    assert_eq!(p.status, Status::Loading);
    let calls = p.take_calls();
    assert!(matches!(&calls[0], Call::Load { path, play: true, .. } if path == "0.mp3"));
    p.loaded();
    assert_eq!(p.status, Status::Playing);
    let calls = p.take_calls();
    assert!(matches!(calls.last(), Some(Call::Preload(Some((_, path)))) if path == "1.mp3"), "{calls:?}");
    assert!(p.drain_notes().contains(&Note::TrackChanged));
}

#[test]
fn play_pause_toggle() {
    let (mut p, ids) = player(2);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    p.take_calls();
    p.command(Command::PlayPause, CommandSource::Ui);
    assert_eq!(p.status, Status::Paused);
    p.command(Command::PlayPause, CommandSource::Ui);
    assert_eq!(p.status, Status::Playing);
    assert_eq!(p.take_calls(), [Call::Paused(true), Call::Paused(false)]);
}

#[test]
fn play_from_stopped_starts_first_entry() {
    let (mut p, _ids) = player(2);
    p.command(Command::PlayPause, CommandSource::System);
    assert_eq!(p.current_path(), "0.mp3");
    assert_eq!(p.status, Status::Loading);
}

#[test]
fn track_end_advances_reusing_preload_token() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    let calls = p.take_calls();
    let Some(Call::Preload(Some((preload_token, _)))) = calls.last().cloned() else { panic!("{calls:?}") };
    let t = p.cur_token();
    p.on_engine_event(EngineEvent::Finished { token: t });
    assert_eq!(p.current_path(), "1.mp3");
    let calls = p.take_calls();
    assert!(matches!(last_load(&calls), Some(Call::Load { token, .. }) if *token == preload_token));
}

#[test]
fn gapless_transition_switches_without_load() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    let Some(Call::Preload(Some((token, _)))) = p.take_calls().last().cloned() else { panic!() };
    p.on_engine_event(EngineEvent::TrackStarted { token, info: info() });
    assert_eq!(p.current_path(), "1.mp3");
    assert_eq!(p.status, Status::Playing);
    let calls = p.take_calls();
    assert!(last_load(&calls).is_none(), "no reload: {calls:?}");
    assert!(matches!(calls.last(), Some(Call::Preload(Some((_, path)))) if path == "2.mp3"));
}

#[test]
fn stale_events_are_ignored() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    let old = p.cur_token();
    p.play_entry(0, ids[2], 0.0);
    p.on_engine_event(EngineEvent::Loaded { token: old, info: info() });
    assert_eq!(p.status, Status::Loading, "old Loaded must not mark the new track playing");
    p.on_engine_event(EngineEvent::Finished { token: old });
    assert_eq!(p.current_path(), "2.mp3", "old Finished must not advance");
    p.engine.pos.set((old, 150.0));
    assert_eq!(p.position(), 0.0, "position of another token is not shown");
}

#[test]
fn load_failure_marks_entry_and_moves_on() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    let t = p.cur_token();
    p.on_engine_event(EngineEvent::LoadFailed { token: t, error: LoadError::NotFound });
    assert_eq!(p.playlist(0).get(ids[0]).unwrap().status, EntryStatus::Missing);
    assert_eq!(p.current_path(), "1.mp3");
    assert!(p.drain_notes().iter().any(|n| matches!(n, Note::Error(_))));
}

#[test]
fn all_broken_stops_instead_of_looping() {
    let (mut p, ids) = player(3);
    p.set_repeat(RepeatMode::All);
    p.play_entry(0, ids[0], 0.0);
    for _ in 0..10 {
        if p.status == Status::Stopped {
            break;
        }
        let t = p.cur_token();
        p.on_engine_event(EngineEvent::LoadFailed { token: t, error: LoadError::Unsupported("x".into()) });
    }
    assert_eq!(p.status, Status::Stopped);
}

#[test]
fn previous_restarts_after_three_seconds() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[1], 0.0);
    p.loaded();
    p.take_calls();
    p.engine.pos.set((p.cur_token(), 42.0));
    p.command(Command::Previous, CommandSource::Ui);
    assert_eq!(p.take_calls(), [Call::Seek(0.0)]);
    p.engine.pos.set((p.cur_token(), 1.0));
    p.on_engine_event(EngineEvent::Seeked { token: p.cur_token(), position: 0.0 });
    // Same source: a different source within 150 ms would be dropped as a duplicate.
    p.command(Command::Previous, CommandSource::Ui);
    assert_eq!(p.current_path(), "0.mp3");
}

#[test]
fn removing_the_current_entry_keeps_playing_and_continues_after_it() {
    let (mut p, ids) = player(4);
    p.play_entry(0, ids[1], 0.0);
    p.loaded();
    p.take_calls();
    p.remove_entries(0, &HashSet::from([ids[1]]));
    assert_eq!(p.status, Status::Playing);
    assert!(!p.take_calls().iter().any(|c| matches!(c, Call::Stop | Call::Load { .. })));
    p.command(Command::Next, CommandSource::Ui);
    assert_eq!(p.current_path(), "2.mp3");
}

#[test]
fn editing_the_list_updates_the_preload() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    p.take_calls();
    p.edit_list(0, |l| {
        l.move_to(&HashSet::from([ids[2]]), 1);
    });
    let calls = p.take_calls();
    assert!(matches!(calls.last(), Some(Call::Preload(Some((_, path)))) if path == "2.mp3"), "{calls:?}");
}

#[test]
fn media_key_delivered_twice_toggles_once() {
    let (mut p, ids) = player(2);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    p.command(Command::PlayPause, CommandSource::System);
    p.command(Command::PlayPause, CommandSource::Keyboard);
    assert_eq!(p.status, Status::Paused);
}

#[test]
fn repeat_one_replays_on_end() {
    let (mut p, ids) = player(3);
    p.set_repeat(RepeatMode::One);
    p.play_entry(0, ids[1], 0.0);
    p.loaded();
    let t = p.cur_token();
    p.on_engine_event(EngineEvent::Finished { token: t });
    assert_eq!(p.current_path(), "1.mp3");
    assert_ne!(p.cur_token(), t, "a fresh load");
}

#[test]
fn skip_while_paused_stays_paused() {
    let (mut p, ids) = player(3);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    p.command(Command::Pause, CommandSource::Ui);
    p.take_calls();
    p.command(Command::Next, CommandSource::Ui);
    let calls = p.take_calls();
    assert!(matches!(last_load(&calls), Some(Call::Load { play: false, .. })));
    p.loaded();
    assert_eq!(p.status, Status::Paused);
}

#[test]
fn seek_while_stopped_loads_paused_at_position() {
    let (mut p, ids) = player(2);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    p.command(Command::Stop, CommandSource::Ui);
    p.take_calls();
    p.command(Command::SeekTo(30.0), CommandSource::Ui);
    let calls = p.take_calls();
    assert!(matches!(last_load(&calls), Some(Call::Load { start_at, play: false, .. }) if *start_at == 30.0));
    assert_eq!(p.position(), 30.0);
}

#[test]
fn seek_clamps_and_reports_pending_position() {
    let (mut p, ids) = player(1);
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    p.take_calls();
    p.command(Command::SeekTo(500.0), CommandSource::System);
    assert_eq!(p.take_calls(), [Call::Seek(200.0)]);
    assert_eq!(p.position(), 200.0);
    p.command(Command::SeekBy(-1000.0), CommandSource::Ui);
    assert_eq!(p.take_calls(), [Call::Seek(0.0)]);
}

#[test]
fn end_of_list_stops() {
    let (mut p, ids) = player(2);
    p.play_entry(0, ids[1], 0.0);
    p.loaded();
    let t = p.cur_token();
    p.on_engine_event(EngineEvent::Finished { token: t });
    assert_eq!(p.status, Status::Stopped);
    assert_eq!(p.current_path(), "1.mp3", "the last track stays selected");
}

#[test]
fn playing_list_cannot_be_removed() {
    let (mut p, ids) = player(2);
    p.add_list(PlaylistDoc { key: "b".into(), playlist: Playlist::new("B") });
    p.play_entry(0, ids[0], 0.0);
    p.loaded();
    assert!(p.remove_list(0).is_none());
    assert!(p.remove_list(1).is_some());
    p.command(Command::Stop, CommandSource::Ui);
    p.add_list(PlaylistDoc { key: "c".into(), playlist: Playlist::new("C") });
    assert!(p.remove_list(0).is_some());
    assert!(p.current.is_none());
}

#[test]
fn copy_entries_appends_independent_copies_in_source_order() {
    let (mut p, ids) = player(3);
    p.add_list(PlaylistDoc { key: "b".into(), playlist: Playlist::new("b") });
    let wanted: HashSet<EntryId> = [ids[2], ids[0]].into();
    assert_eq!(p.copy_entries(0, &wanted, 1), 2);
    let copies = p.playlist(1).entries();
    assert_eq!(copies.iter().map(|e| e.path.display().to_string()).collect::<Vec<_>>(), ["0.mp3", "2.mp3"]);
    assert!(copies.iter().all(|c| !ids.contains(&c.id)));
    assert_eq!(p.playlist(0).len(), 3);
    assert_eq!(p.copy_entries(0, &wanted, 0), 0);
    assert!(p.drain_notes().contains(&Note::PlaylistsChanged));
}
