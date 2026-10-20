use super::*;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/rekordbox")
}

fn stick() -> Stick {
    Stick::open(&fixture()).expect("fixture opens")
}

fn track<'a>(stick: &'a Stick, title: &str) -> &'a Track {
    stick.tracks.iter().find(|t| t.title == title).unwrap_or_else(|| panic!("no track {title}"))
}

#[test]
fn reads_all_tracks_with_resolved_names() {
    let stick = stick();
    assert_eq!(stick.tracks.len(), 3);
    let t = track(&stick, "Carte Blanche (Original Mix)");
    assert_eq!((t.artist.as_str(), t.album.as_str()), ("Veracocha", "Carte Blanche"));
    assert_eq!((t.genre.as_str(), t.label.as_str()), ("Dance; Trance", "Deal Records"));
    assert_eq!(t.key.as_deref(), Some("Bbm"));
    assert_eq!(t.bpm, Some(140.02));
    assert_eq!((t.duration_secs, t.year, t.bitrate, t.sample_rate), (425, Some(1999), 320, 44100));
}

#[test]
fn keeps_non_ascii_names() {
    let stick = stick();
    assert_eq!(track(&stick, "Suburban Train").artist, "Tiësto");
}

#[test]
fn paths_resolve_against_the_stick_root() {
    let stick = stick();
    let t = track(&stick, "Second Wave");
    assert_eq!(t.file, fixture().join("Contents/Signum/Second Wave 2004/Signum - Second Wave.mp3"));
    assert_eq!(t.analysis, Some(fixture().join("PIONEER/USBANLZ/P002/0000D906/ANLZ0000.DAT")));
    assert_eq!(t.artwork, Some(fixture().join("PIONEER/Artwork/00001/a2.jpg")));
}

#[test]
fn detail_waveform_has_150_columns_per_second_with_colours() {
    let stick = stick();
    let t = track(&stick, "Carte Blanche (Original Mix)");
    let wave = t.waveform().unwrap().expect("analysed");
    let seconds = wave.heights.len() as f32 / 150.0;
    assert!((seconds - t.duration_secs as f32).abs() < 1.5, "{seconds} s for {} s", t.duration_secs);
    assert!(wave.heights.iter().all(|&h| h <= 31));
    assert!(wave.heights.iter().any(|&h| h > 10), "a loud track has tall columns");
    assert_eq!(wave.colors.as_ref().map(Vec::len), Some(wave.heights.len()));
}

#[test]
fn colour_and_plain_detail_waveforms_agree() {
    let ext = std::fs::read(fixture().join("PIONEER/USBANLZ/P011/00020633/ANLZ0000.EXT")).unwrap();
    let colour = anlz::detail(&ext).unwrap().unwrap();
    let plain = anlz::section(&ext, b"PWV3").unwrap().unwrap();
    assert_eq!(colour.heights.len(), plain.len());
    // rekordbox computes the two independently, so a few columns differ; the overall shape must not.
    let differing = colour.heights.iter().zip(plain).filter(|(a, b)| **a != **b & 0x1f).count();
    assert!(differing * 10 < plain.len(), "{differing} of {} columns differ", plain.len());
    let mean = |v: &mut dyn Iterator<Item = u8>| v.map(f32::from).sum::<f32>() / plain.len() as f32;
    let (a, b) = (mean(&mut colour.heights.iter().copied()), mean(&mut plain.iter().map(|b| b & 0x1f)));
    assert!((a - b).abs() < 0.2, "mean height {a} against {b}");
}

#[test]
fn bucket_colors_follow_the_buckets_and_use_the_full_brightness() {
    let stick = stick();
    let wave = track(&stick, "Carte Blanche (Original Mix)").waveform().unwrap().unwrap();
    let colors = wave.bucket_colors(2048).expect("colour waveform");
    assert_eq!(colors.len(), wave.buckets(2048).0.len());
    // Silent buckets have no colour and come out grey.
    let coloured: Vec<_> = colors.iter().filter(|c| **c != [128; 3]).collect();
    assert!(coloured.len() * 10 > colors.len() * 9);
    assert!(coloured.iter().all(|c| c.contains(&255)));
    assert!(colors.iter().any(|c| c[0] > c[2]) && colors.iter().any(|c| c[2] > c[0]), "both bass and treble colours occur");
    let plain = Waveform { heights: vec![5; 10], colors: None };
    assert_eq!(plain.bucket_colors(4), None);
}

#[test]
fn preview_waveform_has_400_columns() {
    let dat = std::fs::read(fixture().join("PIONEER/USBANLZ/P011/00020633/ANLZ0000.DAT")).unwrap();
    let wave = anlz::preview(&dat).unwrap().unwrap();
    assert_eq!(wave.heights.len(), 400);
}

#[test]
fn track_whose_analysis_files_are_missing_has_no_waveform() {
    let stick = stick();
    assert_eq!(track(&stick, "Second Wave").waveform().unwrap(), None);
}

#[test]
fn buckets_scale_to_the_full_range_and_never_exceed_the_columns() {
    let wave = Waveform { heights: vec![0, 31, 31, 0, 8, 8], colors: None };
    let (peak, rms) = wave.buckets(3);
    assert_eq!(peak, vec![255, 255, 66]);
    assert_eq!(rms, vec![180, 180, 66]);
    assert_eq!(wave.buckets(100).0.len(), 6);
}

#[test]
fn truncated_or_foreign_files_are_errors() {
    assert!(matches!(pdb::read(b"not a database"), Err(Error::Format(_))));
    let good = std::fs::read(fixture().join("PIONEER/rekordbox/export.pdb")).unwrap();
    for cut in [10, 100, 4096 * 3 + 7, good.len() / 2] {
        let _ = pdb::read(&good[..cut]);
    }
    assert!(anlz::detail(b"nope").is_err());
}

#[test]
fn rating_and_colour_label_come_from_the_track_row() {
    let mut data = std::fs::read(fixture().join("PIONEER/rekordbox/export.pdb")).unwrap();
    let stick = stick();
    assert!(stick.tracks.iter().all(|t| t.rating == 0 && t.color.is_none()), "the fixture's tracks have neither");
    // Set both on the first track row (colour at offset 88, rating at 89), found through the tracks table.
    let page_size = 4096;
    let first = u32::from_le_bytes(data[28 + 8..28 + 12].try_into().unwrap()) as usize;
    let last = u32::from_le_bytes(data[28 + 12..28 + 16].try_into().unwrap()) as usize;
    let mut patched = 0;
    for page in first..=last {
        let range = page * page_size..(page + 1) * page_size;
        let offsets: Vec<usize> = pdb::page_rows(&data[range.clone()]).iter().map(|row| row.as_ptr() as usize - data.as_ptr() as usize).collect();
        for at in offsets {
            data[at + 88] = 6;
            data[at + 89] = 4;
            patched += 1;
        }
    }
    assert_eq!(patched, 3);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("PIONEER/rekordbox")).unwrap();
    std::fs::write(dir.path().join("PIONEER/rekordbox/export.pdb"), data).unwrap();
    let stick = Stick::open(dir.path()).unwrap();
    assert!(stick.tracks.iter().all(|t| t.rating == 4 && t.color == Some(Color::Aqua)));
    assert_eq!(Color::from_name(Color::Aqua.name()), Some(Color::Aqua));
}

#[test]
fn path_key_ignores_how_accents_are_encoded() {
    assert_eq!(path_key(Path::new("/Contents/Ti\u{eb}sto")), path_key(Path::new("/Contents/Tie\u{308}sto")));
    assert_ne!(path_key(Path::new("/Contents/Tiesto")), path_key(Path::new("/Contents/Ti\u{eb}sto")));
}

#[test]
fn stick_root_is_the_folder_with_the_export() {
    let file = fixture().join("Contents/Signum/Second Wave 2004/Signum - Second Wave.mp3");
    assert_eq!(stick_root(&file), Some(fixture()));
    assert_eq!(stick_root(Path::new("/definitely/not/a/stick/file.mp3")), None);
}

#[test]
fn detect_finds_only_volumes_with_an_export() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("STICK/PIONEER/rekordbox")).unwrap();
    std::fs::write(dir.path().join("STICK/PIONEER/rekordbox/export.pdb"), b"x").unwrap();
    std::fs::create_dir_all(dir.path().join("OTHER/Music")).unwrap();
    assert_eq!(detect(dir.path()), vec![dir.path().join("STICK")]);
}
