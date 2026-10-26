//! Looking for a playlist track's file that is no longer where the playlist says: a name search through
//! one folder tree. Only directory entries are read, never the files, so a library of tens of thousands
//! of files takes a second or two.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::workers::is_audio_file;

/// Folders deeper than this are not entered (the same limit as adding a folder to a playlist).
const MAX_DEPTH: usize = 32;
/// The search stops once this many files with a similar name are found; nobody wants more to choose from.
const MAX_FOUND: usize = 200;
/// What a result lists: the best of what was found.
pub const MAX_SHOWN: usize = 20;

/// How close a file's name is to the missing one's, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Match {
    /// The same file name, apart from the case.
    Name,
    /// The same name in another audio format (a library converted from FLAC to MP3, say).
    Format,
    /// The same words once track numbers, case and punctuation are ignored ("01 - Artist - Title").
    Similar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub kind: Match,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Search {
    /// The best matches first, at most [`MAX_SHOWN`].
    pub found: Vec<Candidate>,
    /// Audio files looked at.
    pub scanned: usize,
    /// Folders that could not be read (no permission, gone meanwhile).
    pub skipped: usize,
    /// More matches than are listed were found.
    pub more: bool,
}

/// Folders no music is kept in and that hold many thousands of files: application and library bundles, and
/// the dependencies of a programming project.
fn skipped_folder(name: &str) -> bool {
    let name = name.to_lowercase();
    name == "node_modules" || name.rsplit_once('.').is_some_and(|(_, ext)| matches!(ext, "app" | "framework" | "bundle" | "photoslibrary" | "xcodeproj" | "xcworkspace"))
}

/// The name without its leading track number, case and punctuation, for comparing names that were tidied differently.
fn words(stem: &str) -> String {
    let rest = stem.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace() || matches!(c, '-' | '_' | '.' | ')'));
    // A name that is nothing but a number ("1999") keeps it.
    let rest = if rest.is_empty() { stem } else { rest };
    rest.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn lower_name(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase())
}

/// How many folders above the file are the same in both paths, nearest first: among several files of one
/// name, the one in an `Artist/Album` folder like the old one is the likely one.
fn shared_folders(old: &Path, new: &Path) -> usize {
    let names = |p: &Path| -> Vec<String> { p.parent().into_iter().flat_map(|d| d.components().rev()).filter_map(|c| matches!(c, Component::Normal(_)).then(|| c.as_os_str().to_string_lossy().to_lowercase())).collect() };
    names(old).iter().zip(names(new)).take_while(|(a, b)| **a == *b).count()
}

/// Look in `root` and below for files that could be `wanted`. `None` when `cancelled` said so meanwhile.
pub fn search(root: &Path, wanted: &Path, cancelled: &dyn Fn() -> bool) -> Option<Search> {
    let name = lower_name(wanted)?;
    let stem = wanted.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let key = words(&stem);
    let mut out = Search::default();
    let mut found: Vec<Candidate> = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if cancelled() {
            return None;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            out.skipped += 1;
            continue;
        };
        for entry in read.filter_map(|e| e.ok()) {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH && !skipped_folder(&file_name) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !is_audio_file(&path) || (kind.is_symlink() && !path.is_file()) {
                continue;
            }
            out.scanned += 1;
            let lower = file_name.to_lowercase();
            let this_stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            let kind = if lower == name {
                Match::Name
            } else if this_stem == stem {
                Match::Format
            } else if !key.is_empty() && words(&this_stem) == key {
                Match::Similar
            } else {
                continue;
            };
            if path != wanted && seen.insert(path.clone()) {
                found.push(Candidate { path, kind });
            }
        }
        if found.len() >= MAX_FOUND {
            break;
        }
    }
    found.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| shared_folders(wanted, &b.path).cmp(&shared_folders(wanted, &a.path))).then_with(|| a.path.cmp(&b.path)));
    out.more = found.len() > MAX_SHOWN;
    found.truncate(MAX_SHOWN);
    out.found = found;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn finds_by_name_then_format_then_similar_name() {
        let dir = tempfile::tempdir().unwrap();
        let exact = touch(dir.path(), "a/Artist - Title.MP3");
        let format = touch(dir.path(), "b/artist - title.flac");
        let similar = touch(dir.path(), "c/01 - Artist_Title.m4a");
        touch(dir.path(), "c/Artist - Other.mp3");
        touch(dir.path(), "c/Artist - Title.txt");
        touch(dir.path(), ".hidden/Artist - Title.mp3");
        touch(dir.path(), "Some.app/Contents/Artist - Title.mp3");
        touch(dir.path(), "node_modules/Artist - Title.mp3");
        let wanted = Path::new("/gone/Artist - Title.mp3");

        let found = search(dir.path(), wanted, &|| false).unwrap();
        let paths: Vec<_> = found.found.iter().map(|c| (&c.path, c.kind)).collect();
        assert_eq!(paths, vec![(&exact, Match::Name), (&format, Match::Format), (&similar, Match::Similar)]);
        assert_eq!(found.scanned, 4);
        assert!(!found.more);
    }

    #[test]
    fn same_name_in_the_album_folder_of_the_old_path_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "Compilations/Hits/Song.mp3");
        let right = touch(dir.path(), "Artist/Album/Song.mp3");
        let found = search(dir.path(), Path::new("/old/Artist/Album/Song.mp3"), &|| false).unwrap();
        assert_eq!(found.found[0].path, right);
        assert_eq!(found.found.len(), 2);
    }

    #[test]
    fn a_name_that_is_a_number_still_matches_itself() {
        assert_eq!(words("1999"), "1999");
        assert_eq!(words("02. Prince - 1999"), "prince1999");
        assert_eq!(words("Prince - 1999"), "prince1999");
    }

    #[test]
    fn the_old_path_itself_is_not_offered_and_cancelling_stops() {
        let dir = tempfile::tempdir().unwrap();
        let same = touch(dir.path(), "x/Song.mp3");
        assert!(search(dir.path(), &same, &|| false).unwrap().found.is_empty());
        assert!(search(dir.path(), &same, &|| true).is_none());
    }

    #[test]
    fn many_matches_are_cut_to_the_best() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..30 {
            touch(dir.path(), &format!("d{i}/Song.mp3"));
        }
        let found = search(dir.path(), Path::new("/old/Song.mp3"), &|| false).unwrap();
        assert_eq!((found.found.len(), found.more), (MAX_SHOWN, true));
    }
}
