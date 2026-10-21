//! Looks for a Python 3, which the "Copy key command" recipe on the rekordbox tab needs. It starts
//! at most a few short processes, so it runs on a helper thread like the other checks.

use std::path::PathBuf;
use std::process::Command;

/// The version of the first Python 3 found ("3.12.4"), `None` when there is none.
///
/// The app may be launched from Finder with a bare `PATH` that lacks the Homebrew folders the user's
/// Terminal has, so those are tried first. On macOS `/usr/bin/python3` is a stub that opens an install
/// dialog when the command line tools are missing, so it only runs once they are known to be there.
pub fn detect() -> Option<String> {
    candidates().into_iter().find_map(|python| version_of(&python))
}

fn candidates() -> Vec<PathBuf> {
    if cfg!(windows) {
        // `py` is the launcher python.org installs; `python` may be the Store's stub, which prints nothing.
        return vec!["python".into(), "py".into()];
    }
    let mut dirs: Vec<PathBuf> = vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into()];
    dirs.extend(std::env::var_os("PATH").iter().flat_map(std::env::split_paths));
    let stub = PathBuf::from("/usr/bin");
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in dirs.into_iter().filter(|d| *d != stub) {
        let python = dir.join("python3");
        if python.is_file() && !found.contains(&python) {
            found.push(python);
        }
    }
    let system = stub.join("python3");
    if system.is_file() && (!cfg!(target_os = "macos") || command_line_tools_installed()) {
        found.push(system);
    }
    found
}

fn command_line_tools_installed() -> bool {
    Command::new("xcode-select").arg("-p").output().is_ok_and(|o| o.status.success())
}

fn version_of(python: &PathBuf) -> Option<String> {
    let out = Command::new(python).arg("--version").output().ok().filter(|o| o.status.success())?;
    // Python 2 printed to stderr and 3.4 and later print to stdout; take whichever has the text.
    let text = if out.stdout.is_empty() { out.stderr } else { out.stdout };
    parse_version(&String::from_utf8_lossy(&text))
}

/// "3.12.4" from "Python 3.12.4", `None` for anything that is not Python 3.
fn parse_version(text: &str) -> Option<String> {
    let version = text.trim().strip_prefix("Python ")?.split_whitespace().next()?;
    version.starts_with("3.").then(|| version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_of_python_3_only() {
        assert_eq!(parse_version("Python 3.12.4\n").as_deref(), Some("3.12.4"));
        assert_eq!(parse_version("Python 3.13.0rc1").as_deref(), Some("3.13.0rc1"));
        assert_eq!(parse_version("Python 2.7.18"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("command not found"), None);
    }
}
