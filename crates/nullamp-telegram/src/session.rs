//! The login kept between runs: which datacenter is home and the permanent key for each, in a
//! JSON file only the user can read. The key is as good as a logged-in device, so it never goes
//! anywhere else, and signing out deletes the file.

use std::io;
use std::path::Path;

use grammers_session::storages::MemorySession;
use grammers_session::types::DcOption;
use grammers_session::{Session, SessionData};
use serde::{Deserialize, Serialize};

/// The datacenters Telegram's clients know statically; a session holds a key for any of them.
const DCS: std::ops::RangeInclusive<i32> = 1..=5;

#[derive(Serialize, Deserialize)]
struct Saved {
    home_dc: i32,
    dc_options: Vec<DcOption>,
}

/// The saved session, or a fresh one when there is none or it cannot be read (the user then signs in again).
pub fn load(path: &Path) -> SessionData {
    let mut data = SessionData::default();
    match std::fs::read(path).map(|b| serde_json::from_slice::<Saved>(&b)) {
        Ok(Ok(saved)) => {
            data.home_dc = saved.home_dc;
            for option in saved.dc_options {
                data.dc_options.insert(option.id, option);
            }
        }
        Ok(Err(e)) => log::warn!("Telegram: the session file {} is unreadable ({e}); signing in again is needed", path.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("Telegram: cannot read the session file {}: {e}", path.display()),
    }
    data
}

/// Write the session through a temporary file, readable by the user only.
pub fn save(path: &Path, session: &MemorySession) -> io::Result<()> {
    let other = |e: &dyn std::fmt::Display| io::Error::other(e.to_string());
    let home_dc = session.home_dc_id().map_err(|e| other(&e))?;
    let mut dc_options = Vec::new();
    for id in DCS {
        if let Some(option) = session.dc_option(id).map_err(|e| other(&e))? {
            dc_options.push(option);
        }
    }
    let bytes = serde_json::to_vec(&Saved { home_dc, dc_options }).map_err(|e| other(&e))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    write_private(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

/// Whether a session file exists, i.e. an earlier sign-in may still be good.
pub fn exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_and_home_datacenter_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        assert_eq!(load(&path).home_dc, SessionData::default().home_dc);

        let mut data = SessionData::default();
        data.home_dc = 4;
        data.dc_options.get_mut(&4).unwrap().auth_key = Some([7; 256]);
        save(&path, &MemorySession::from(data)).unwrap();

        let back = load(&path);
        assert_eq!(back.home_dc, 4);
        assert_eq!(back.dc_options[&4].auth_key, Some([7; 256]));
        assert_eq!(back.dc_options[&2].auth_key, None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn a_broken_file_means_a_fresh_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(load(&path).home_dc, SessionData::default().home_dc);
    }
}
