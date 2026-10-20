//! Where Nullamp keeps its files.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct AppPaths {
    /// Settings, playlists and saved setlists.
    pub config: PathBuf,
    /// Tag-write backups.
    pub data: PathBuf,
    /// Artwork thumbnails and waveform overviews; safe to delete.
    pub cache: PathBuf,
}

impl AppPaths {
    pub fn detect() -> Self {
        if let Ok(dir) = std::env::var("NULLAMP_HOME") {
            // Portable / test mode: everything under one directory.
            let base = PathBuf::from(dir);
            return AppPaths { config: base.join("config"), data: base.join("data"), cache: base.join("cache") };
        }
        match directories::ProjectDirs::from("", "", "Nullamp") {
            Some(d) => AppPaths {
                config: d.config_dir().to_path_buf(),
                data: d.data_dir().to_path_buf(),
                cache: d.cache_dir().to_path_buf(),
            },
            None => {
                let base = std::env::temp_dir().join("nullamp");
                AppPaths { config: base.join("config"), data: base.join("data"), cache: base.join("cache") }
            }
        }
    }

    pub fn playlists(&self) -> PathBuf {
        self.config.join("playlists")
    }

    pub fn setlists(&self) -> PathBuf {
        self.config.join("setlists")
    }

    pub fn tag_backups(&self) -> PathBuf {
        self.data.join("tag-backups")
    }

    pub fn artwork_cache(&self) -> PathBuf {
        self.cache.join("artwork")
    }

    /// Covers found online for the tracks inside mixes.
    pub fn track_artwork_cache(&self) -> PathBuf {
        self.artwork_cache().join("tracks")
    }

    pub fn waveform_cache(&self) -> PathBuf {
        self.cache.join("waveforms")
    }
}
