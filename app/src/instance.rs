//! One running Nullamp per config folder. A second launch hands its files to the first and exits, so
//! `nullamp song.mp3` and a file manager's "Open with" reuse the window instead of racing it for the settings.
//!
//! On Unix the first instance listens on a socket; on Windows there is no such channel yet and every launch
//! is its own instance. (On macOS, Finder already routes a document to the running app; this covers the
//! command line and `open -n`.)

use std::path::{Path, PathBuf};

pub enum Role {
    /// This process runs the app. It owns the listener when one could be set up.
    Primary(Option<Listener>),
    /// The files went to the running instance; this process should exit.
    Forwarded,
}

#[cfg(unix)]
pub use unix::{Listener, claim};

#[cfg(not(unix))]
pub struct Listener;

#[cfg(not(unix))]
impl Listener {
    pub fn spawn(self) {}
}

#[cfg(not(unix))]
pub fn claim(_config: &Path, _paths: &[PathBuf]) -> Role {
    Role::Primary(None)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::Duration;

    /// A socket path must stay under about 100 bytes, which a config folder inside `Application Support` or a
    /// portable install can exceed, so the name is a hash of the config folder in the runtime or temp folder.
    fn socket_path(config: &Path) -> PathBuf {
        let hash = config.as_os_str().as_bytes().iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
        dir.join(format!("nullamp-{hash:016x}.sock"))
    }

    pub struct Listener(UnixListener);

    impl Listener {
        /// Accept requests on a thread of its own and queue the files they name.
        pub fn spawn(self) {
            let spawned = std::thread::Builder::new().name("nullamp-instance".into()).spawn(move || {
                for stream in self.0.incoming().flatten() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = Vec::new();
                    // A path list of a few MiB is already far more than anyone opens at once.
                    if stream.take(4 << 20).read_to_end(&mut buf).is_ok() {
                        crate::open::push(decode(&buf));
                    }
                }
            });
            if let Err(e) = spawned {
                log::warn!("single-instance listener not started: {e}");
            }
        }
    }

    /// Paths separated by NUL bytes: the one byte a path cannot contain, so no escaping is needed.
    fn encode(paths: &[PathBuf]) -> Vec<u8> {
        paths.iter().flat_map(|p| p.as_os_str().as_bytes().iter().copied().chain([0])).collect()
    }

    fn decode(buf: &[u8]) -> Vec<PathBuf> {
        buf.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| PathBuf::from(std::ffi::OsString::from_vec(s.to_vec()))).collect()
    }

    pub fn claim(config: &Path, paths: &[PathBuf]) -> Role {
        let socket = socket_path(config);
        if let Ok(mut running) = UnixStream::connect(&socket) {
            match running.write_all(&encode(paths)) {
                Ok(()) => return Role::Forwarded,
                Err(e) => log::warn!("cannot reach the running instance: {e}"),
            }
        }
        // Nobody answers: a stale file from a crash, or no file at all.
        let _ = std::fs::remove_file(&socket);
        match UnixListener::bind(&socket) {
            Ok(listener) => {
                // Anything that can write to the socket can make the player open files, so only its owner may.
                let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
                Role::Primary(Some(Listener(listener)))
            }
            Err(e) => {
                log::warn!("single-instance socket {} not created: {e}", socket.display());
                Role::Primary(None)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn paths_survive_the_wire() {
            let paths = vec![PathBuf::from("/a b/ü.mp3"), PathBuf::from("/c.flac")];
            assert_eq!(decode(&encode(&paths)), paths);
            assert!(decode(&encode(&[])).is_empty());
        }

        #[test]
        fn a_second_claim_forwards_to_the_first() {
            let dir = tempfile::tempdir().unwrap();
            let first = claim(dir.path(), &[]);
            assert!(matches!(first, Role::Primary(Some(_))));
            assert!(matches!(claim(dir.path(), &[PathBuf::from("/x.mp3")]), Role::Forwarded));
            // A different config folder is a different instance.
            let other = tempfile::tempdir().unwrap();
            assert!(matches!(claim(other.path(), &[]), Role::Primary(Some(_))));
        }
    }
}
