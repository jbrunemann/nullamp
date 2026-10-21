use super::*;

/// A library with the tables and columns Mixxx 2.3 writes (the ones read here), a deleted track,
/// a crate Mixxx hides, and the three kinds of playlist.
fn synthetic_library() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mixxxdb.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE track_locations(id INTEGER PRIMARY KEY AUTOINCREMENT, location varchar(512) UNIQUE, filename varchar(512), directory varchar(512), filesize INTEGER, fs_deleted INTEGER, needs_verification INTEGER);
         CREATE TABLE library(id INTEGER primary key AUTOINCREMENT, artist varchar(64), title varchar(64), album varchar(64), year varchar(16), genre varchar(64), tracknumber varchar(3), location integer, comment varchar(256), duration float, bitrate integer, samplerate integer, bpm float, datetime_added DEFAULT CURRENT_TIMESTAMP, mixxx_deleted integer, timesplayed integer DEFAULT 0, rating integer DEFAULT 0, key varchar(8) DEFAULT '');
         CREATE TABLE crates(id integer PRIMARY KEY AUTOINCREMENT, name varchar(48) UNIQUE NOT NULL, count integer DEFAULT 0, show integer DEFAULT 1, locked integer DEFAULT 0, autodj_source integer DEFAULT 0);
         CREATE TABLE crate_tracks(crate_id integer NOT NULL, track_id integer NOT NULL, UNIQUE (crate_id, track_id));
         CREATE TABLE Playlists(id INTEGER primary key, name varchar(48), position INTEGER, hidden INTEGER DEFAULT 0 NOT NULL, date_created datetime, date_modified datetime, locked integer DEFAULT 0);
         CREATE TABLE PlaylistTracks(id INTEGER primary key, playlist_id INTEGER, track_id INTEGER, position INTEGER, pl_datetime_added);
         INSERT INTO track_locations(id, location) VALUES (1,'/home/x/Music/a.mp3'), (2,'/home/x/Music/b.mp3'), (3,'/home/x/Music/gone.mp3'), (4,'');
         INSERT INTO library(id,artist,title,album,year,genre,tracknumber,location,comment,duration,bitrate,samplerate,bpm,datetime_added,mixxx_deleted,timesplayed,rating,key)
             VALUES (1,'Alpha Artist','Alpha','Alpha Album','1999','Trance','3/12',1,'hi',425.5,320,44100,128.5,'2019-06-24T11:41:33Z',0,7,5,'B♭m'),
                    (2,NULL,'Beta',NULL,NULL,NULL,NULL,2,NULL,100,NULL,NULL,0.0,NULL,NULL,0,0,''),
                    (3,'Gone','Gone',NULL,NULL,NULL,NULL,3,NULL,1,0,0,0,NULL,1,0,0,''),
                    (4,'No','Location',NULL,NULL,NULL,NULL,4,NULL,1,0,0,0,NULL,0,0,0,'');
         UPDATE library SET year = 2001, tracknumber = 4 WHERE id = 2;
         INSERT INTO crates(id,name,show) VALUES (1,'zeta',1), (2,'Alpha',1), (3,'Hidden',0), (4,'Empty',1);
         INSERT INTO crate_tracks VALUES (1,2), (1,1), (2,1), (2,3), (3,1);
         INSERT INTO Playlists(id,name,position,hidden) VALUES (1,'Auto DJ',1,1), (2,'2019-06-24',2,2), (3,'Warm up',3,0), (4,'2019-06-27',4,2);
         INSERT INTO PlaylistTracks(playlist_id,track_id,position) VALUES (3,2,2), (3,1,1), (3,3,3), (2,1,1), (2,2,2), (1,2,1);",
    )
    .unwrap();
    (dir, db)
}

#[test]
fn tracks_are_read_with_mixxxs_loose_types() {
    let (_dir, db) = synthetic_library();
    let lib = Library::open(&db).unwrap();
    assert_eq!(lib.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2], "the deleted track and the one without a location are left out");
    let a = &lib.tracks[0];
    assert_eq!((a.title.as_str(), a.artist.as_str(), a.album.as_str(), a.genre.as_str()), ("Alpha", "Alpha Artist", "Alpha Album", "Trance"));
    assert_eq!((a.key.as_deref(), a.bpm, a.year, a.track_number), (Some("B♭m"), Some(128.5), Some(1999), 3));
    assert_eq!((a.duration_secs, a.bitrate, a.sample_rate, a.rating, a.play_count), (425.5, 320, 44100, 5, 7));
    assert_eq!((a.file.as_path(), a.comment.as_str()), (Path::new("/home/x/Music/a.mp3"), "hi"));
    let b = &lib.tracks[1];
    assert_eq!((b.artist.as_str(), b.key.as_deref(), b.bpm), ("", None, None), "no tempo is 0, not 0 BPM");
    assert_eq!((b.year, b.track_number), (Some(2001), 4), "numbers stored as numbers in text columns");
}

#[test]
fn lists_are_crates_then_playlists_then_history() {
    let (_dir, db) = synthetic_library();
    let lib = Library::open(&db).unwrap();
    let names: Vec<(&str, Kind)> = lib.lists.iter().map(|l| (l.name.as_str(), l.kind)).collect();
    assert_eq!(
        names,
        [("Alpha", Kind::Crate), ("Empty", Kind::Crate), ("zeta", Kind::Crate), ("Auto DJ", Kind::Playlist), ("Warm up", Kind::Playlist), ("2019-06-24", Kind::History), ("2019-06-27", Kind::History)],
        "a crate Mixxx hides is left out, crates sort by name",
    );
    let ids = |name: &str| -> Vec<i64> { lib.lists.iter().find(|l| l.name == name).unwrap().tracks.iter().map(|&i| lib.tracks[i].id).collect() };
    assert_eq!(ids("zeta"), vec![2, 1], "crate order is the order tracks were added");
    assert_eq!(ids("Alpha"), vec![1], "a removed track drops out of a crate");
    assert_eq!(ids("Warm up"), vec![1, 2], "playlist order is the position column");
    assert_eq!(ids("2019-06-24"), vec![1, 2]);
    assert!(ids("Empty").is_empty());
}

#[test]
fn a_missing_or_foreign_file_is_an_error_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Library::open(&dir.path().join("none.sqlite")).is_err());
    let other = dir.path().join("other.sqlite");
    Connection::open(&other).unwrap().execute_batch("CREATE TABLE t(x)").unwrap();
    assert!(Library::open(&other).is_err(), "a database without Mixxx's tables");
    let (_dir, db) = synthetic_library();
    let before = std::fs::read(&db).unwrap();
    Library::open(&db).unwrap();
    assert_eq!(std::fs::read(&db).unwrap(), before);
}
