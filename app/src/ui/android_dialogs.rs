//! Android has no native file dialog Rust can reach. This stands in for `rfd` there (`dialogs.rs` imports it under that
//! name) and asks the in-window file browser instead; the helper thread that runs the dialog waits for the answer.

use std::path::{Path, PathBuf};

use super::file_browser::{self, Mode, Request};

pub struct FileHandle(PathBuf);

impl FileHandle {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

pub struct AsyncFileDialog {
    title: String,
    extensions: Vec<String>,
    dir: Option<PathBuf>,
    name: String,
}

impl AsyncFileDialog {
    pub fn new() -> Self {
        AsyncFileDialog { title: String::new(), extensions: Vec::new(), dir: None, name: String::new() }
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// The filters of a desktop dialog are alternatives to choose from; here they add up.
    pub fn add_filter(mut self, _: impl Into<String>, extensions: &[&str]) -> Self {
        self.extensions.extend(extensions.iter().map(|e| e.to_lowercase()));
        self
    }

    pub fn set_directory(mut self, dir: impl AsRef<Path>) -> Self {
        self.dir = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn set_file_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    async fn ask(self, mode: Mode) -> Option<Vec<FileHandle>> {
        let (reply, answer) = crossbeam_channel::bounded(1);
        file_browser::submit(Request { title: self.title, extensions: self.extensions, dir: self.dir, name: self.name, mode, reply });
        // This runs on a helper thread of its own, so waiting blocks nothing else.
        let paths = answer.recv().ok()??;
        Some(paths.into_iter().map(FileHandle).collect())
    }

    pub async fn pick_folders(self) -> Option<Vec<FileHandle>> {
        self.ask(Mode::Folder).await
    }

    pub async fn pick_folder(self) -> Option<FileHandle> {
        self.ask(Mode::Folder).await?.pop()
    }

    pub async fn pick_files(self) -> Option<Vec<FileHandle>> {
        self.ask(Mode::Files { multi: true }).await
    }

    pub async fn pick_file(self) -> Option<FileHandle> {
        self.ask(Mode::Files { multi: false }).await?.pop()
    }

    pub async fn save_file(self) -> Option<FileHandle> {
        self.ask(Mode::Save).await?.pop()
    }
}
