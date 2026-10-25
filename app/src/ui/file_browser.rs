//! A file browser inside the window, for platforms where the native file dialog is out of reach. Android's own
//! picker hands its answer back through Java, which this app does not have; with the "all files" permission a plain
//! directory listing does the job. The `rfd` stand-in in `dialogs.rs` sends its questions here.
// Only the Android build asks; elsewhere the tests do.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use crossbeam_channel::{Receiver, Sender};
use egui::{Key, RichText, Sense, pos2, vec2};

use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Pick files, one or several.
    Files { multi: bool },
    /// Pick the folder being shown.
    Folder,
    /// Pick a folder and type a file name.
    Save,
}

/// One question for the user. The answer goes to `reply`: the chosen paths, `None` when cancelled.
pub struct Request {
    pub title: String,
    /// Lowercase extensions that can be picked; empty means any file.
    pub extensions: Vec<String>,
    pub dir: Option<PathBuf>,
    pub name: String,
    pub mode: Mode,
    pub reply: Sender<Option<Vec<PathBuf>>>,
}

static REQUESTS: LazyLock<(Sender<Request>, Receiver<Request>)> = LazyLock::new(crossbeam_channel::unbounded);
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Lets `submit` wake the UI, which is idle until something happens.
pub fn init(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Ask for a browser from any thread.
pub fn submit(request: Request) {
    let _ = REQUESTS.0.send(request);
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

pub fn next_request() -> Option<Request> {
    REQUESTS.1.try_recv().ok()
}

struct Item {
    name: String,
    path: PathBuf,
    dir: bool,
}

enum Listing {
    Loading,
    Ready(Vec<Item>),
    Failed(String),
}

/// What the listing thread sends back.
enum Message {
    Listed(PathBuf, Result<Vec<Item>, String>),
    Places(Vec<(String, PathBuf)>),
}

pub struct Browser {
    request: Request,
    dir: PathBuf,
    listing: Listing,
    places: Vec<(String, PathBuf)>,
    rx: Receiver<Message>,
    tx: Sender<Message>,
    ctx: egui::Context,
    picked: Vec<PathBuf>,
    name: String,
}

impl Browser {
    pub fn open(request: Request, ctx: &egui::Context) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let name = request.name.clone();
        let start: Vec<PathBuf> = request.dir.iter().cloned().chain(default_dirs()).collect();
        let mut browser = Browser { dir: start[0].clone(), request, listing: Listing::Loading, places: Vec::new(), rx, tx, ctx: ctx.clone(), picked: Vec::new(), name };
        browser.list(start);
        let (tx, ctx) = (browser.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Message::Places(places()));
            ctx.request_repaint();
        });
        browser
    }

    /// List the first of `dirs` that can be read, on a helper thread: storage can be slow.
    fn list(&mut self, dirs: Vec<PathBuf>) {
        self.listing = Listing::Loading;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let (extensions, files) = (self.request.extensions.clone(), self.request.mode != Mode::Folder);
        std::thread::spawn(move || {
            let mut first_error = None;
            let mut sent = None;
            for dir in &dirs {
                match read(dir, &extensions, files) {
                    Ok(items) => {
                        sent = Some(Message::Listed(dir.clone(), Ok(items)));
                        break;
                    }
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            let _ = tx.send(sent.unwrap_or_else(|| Message::Listed(dirs[0].clone(), Err(first_error.unwrap_or_default()))));
            ctx.request_repaint();
        });
    }

    fn go(&mut self, dir: PathBuf) {
        self.dir = dir.clone();
        self.list(vec![dir]);
    }

    /// Draw the browser over the whole window. `false` once the user has answered.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Listed(dir, result) => {
                    self.dir = dir;
                    self.listing = match result {
                        Ok(items) => Listing::Ready(items),
                        Err(e) => Listing::Failed(e),
                    };
                }
                Message::Places(places) => self.places = places,
            }
        }
        let mut answer: Option<Option<Vec<PathBuf>>> = None;
        let mut go: Option<PathBuf> = None;
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            answer = Some(None);
        }
        let all = ctx.content_rect();
        let (bars_top, bars_bottom) = crate::system_bar_insets(ctx.pixels_per_point());
        let margin = egui::Margin { left: 10, right: 10, top: 8 + bars_top as i8, bottom: 8 + bars_bottom as i8 };
        let inner = all.size() - vec2(20.0, 16.0 + bars_top + bars_bottom);
        egui::Area::new(egui::Id::NULL.with("file-browser")).order(egui::Order::Foreground).fixed_pos(all.min).show(ctx, |ui| {
            theme::with_dark(ui, |ui| {
                egui::Frame::new().fill(theme::deck_bg()).inner_margin(margin).show(ui, |ui| {
                    ui.set_min_size(inner);
                    ui.set_max_size(inner);
                    ui.horizontal(|ui| {
                        ui.heading(&self.request.title);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                answer = Some(None);
                            }
                        });
                    });
                    if !crate::all_files_access() {
                        access_notice(ui);
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (label, path) in &self.places {
                            if ui.small_button(label).clicked() {
                                go = Some(path.clone());
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let parent = self.dir.parent().map(Path::to_path_buf);
                        if ui.add_enabled(parent.is_some(), egui::Button::new("Up")).clicked() {
                            go = parent;
                        }
                        ui.add(egui::Label::new(RichText::new(tail(&self.dir, 34)).monospace().small().color(theme::text_dim())).truncate());
                    });
                    ui.separator();
                    let footer = 48.0;
                    let list_height = (ui.available_height() - footer).max(60.0);
                    egui::ScrollArea::vertical().auto_shrink(false).max_height(list_height).show(ui, |ui| match &self.listing {
                        Listing::Loading => {
                            ui.label(RichText::new("Reading the folder").color(theme::text_dim()));
                        }
                        Listing::Failed(e) => {
                            ui.label(RichText::new(format!("This folder cannot be read: {e}")).color(theme::accent()));
                        }
                        Listing::Ready(items) if items.is_empty() => {
                            ui.label(RichText::new(if self.request.mode == Mode::Folder { "No folders here" } else { "No folders or matching files here" }).color(theme::text_dim()));
                        }
                        Listing::Ready(items) => {
                            for item in items {
                                let picked = self.picked.contains(&item.path);
                                if item_row(ui, item, picked).clicked() {
                                    if item.dir {
                                        go = Some(item.path.clone());
                                    } else {
                                        match self.request.mode {
                                            Mode::Files { multi: true } => match self.picked.iter().position(|p| *p == item.path) {
                                                Some(at) => {
                                                    self.picked.remove(at);
                                                }
                                                None => self.picked.push(item.path.clone()),
                                            },
                                            Mode::Files { multi: false } => self.picked = vec![item.path.clone()],
                                            Mode::Save => self.name = item.name.clone(),
                                            Mode::Folder => {}
                                        }
                                    }
                                }
                            }
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| match self.request.mode {
                        Mode::Files { multi } => {
                            let count = self.picked.len();
                            let label = match (multi, count) {
                                (_, 0) => "Pick a file".to_owned(),
                                (false, _) => "Open".to_owned(),
                                (true, 1) => "Add 1 file".to_owned(),
                                (true, n) => format!("Add {n} files"),
                            };
                            if ui.add_enabled(count > 0, egui::Button::new(label).min_size(vec2(120.0, 36.0))).clicked() {
                                answer = Some(Some(std::mem::take(&mut self.picked)));
                            }
                            if multi && count > 0 && ui.button("Clear").clicked() {
                                self.picked.clear();
                            }
                        }
                        Mode::Folder => {
                            if ui.add(egui::Button::new("Use this folder").min_size(vec2(160.0, 36.0))).clicked() {
                                answer = Some(Some(vec![self.dir.clone()]));
                            }
                        }
                        Mode::Save => {
                            let valid = !self.name.trim().is_empty() && !self.name.contains(['/', '\\']);
                            if ui.add_enabled(valid, egui::Button::new("Save").min_size(vec2(80.0, 36.0))).clicked() {
                                answer = Some(Some(vec![self.dir.join(self.name.trim())]));
                            }
                            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
                        }
                    });
                });
            });
        });
        if let Some(dir) = go {
            self.go(dir);
        }
        match answer {
            Some(paths) => {
                let _ = self.request.reply.send(paths);
                false
            }
            None => true,
        }
    }
}

/// A row of the list, tall enough for a finger. Folders end in a slash; picked files carry the accent.
fn item_row(ui: &mut egui::Ui, item: &Item, picked: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, 3.0, theme::ACCENT_DIM);
    } else if response.hovered() {
        p.rect_filled(rect, 3.0, theme::LCD_OFF);
    }
    let (text, color) = if item.dir { (format!("{}/", item.name), theme::LCD_FG) } else { (item.name.clone(), if picked { theme::LCD_ACCENT } else { theme::LCD_TEXT }) };
    let galley = p.layout(text, theme::prop(15.0), color, rect.width() - 20.0);
    p.galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y.min(32.0) / 2.0), galley, color);
    p.line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0, theme::LCD_BORDER.gamma_multiply(0.5)));
    response
}

/// The end of `dir`, which tells folders apart, with an ellipsis when the start is cut off.
fn tail(dir: &Path, keep: usize) -> String {
    let text = dir.display().to_string();
    let count = text.chars().count();
    if count <= keep { text } else { format!("…{}", text.chars().skip(count - keep).collect::<String>()) }
}

/// Without "all files" access Android shows only a few folders and refuses writes elsewhere, tags included.
fn access_notice(ui: &mut egui::Ui) {
    egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::LCD_AMBER)).corner_radius(4.0).inner_margin(8.0).show(ui, |ui| {
        ui.label(RichText::new("Nullamp needs access to all files to list your music and to save tags to it.").color(theme::LCD_AMBER));
        if ui.button("Open the permission page").clicked() {
            crate::request_all_files_access();
        }
    });
    ui.add_space(4.0);
}

/// Folders and, by the filter, files of `dir`: folders first, then by name. Hidden names are left out.
fn read(dir: &Path, extensions: &[String], files: bool) -> Result<Vec<Item>, String> {
    let mut items = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // `metadata` follows links, so a link to a folder is a folder.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            items.push(Item { name, path, dir: true });
        } else if files && matches(&path, extensions) {
            items.push(Item { name, path, dir: false });
        }
    }
    items.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(items)
}

fn matches(path: &Path, extensions: &[String]) -> bool {
    extensions.is_empty() || path.extension().is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Where to start when the request names no folder, or one that cannot be read.
fn default_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "android")]
    let dirs = ["/storage/emulated/0/Music", "/storage/emulated/0"].map(PathBuf::from).into_iter().chain(std::env::var_os("NULLAMP_MUSIC").map(PathBuf::from)).collect();
    #[cfg(not(target_os = "android"))]
    let dirs = std::env::var_os("HOME").map(|h| vec![PathBuf::from(h)]).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = dirs;
    dirs.push(PathBuf::from("/"));
    dirs
}

/// The shortcuts above the list: the places music usually is, where they exist.
fn places() -> Vec<(String, PathBuf)> {
    let mut places: Vec<(String, PathBuf)> = Vec::new();
    #[cfg(target_os = "android")]
    {
        places.push(("Music".into(), "/storage/emulated/0/Music".into()));
        places.push(("Download".into(), "/storage/emulated/0/Download".into()));
        places.push(("Internal storage".into(), "/storage/emulated/0".into()));
        if let Some(own) = std::env::var_os("NULLAMP_MUSIC") {
            places.push(("Nullamp".into(), own.into()));
        }
        // Memory cards are folders of /storage named by their volume id.
        if let Ok(volumes) = std::fs::read_dir("/storage") {
            for v in volumes.flatten() {
                let name = v.file_name().to_string_lossy().into_owned();
                if name != "emulated" && name != "self" {
                    places.push((format!("Card {name}"), v.path()));
                }
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        places.push(("Music".into(), home.join("Music")));
        places.push(("Home".into(), home));
    }
    places.retain(|(_, p)| p.is_dir());
    places
}
