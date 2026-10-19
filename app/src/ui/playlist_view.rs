//! The playlist editor: named playlists, a dense virtualized table, selection,
//! drag-to-reorder and keyboard editing.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, Vec2, pos2, vec2};
use nullamp_core::{EntryId, EntryStatus, PlaylistEntry};

use super::widgets::{self, Icon};
use super::{App, SideTab, dialogs, inspector, theme};
use crate::player::Status;
use crate::store::PlaylistDoc;
use crate::workers::{Job, Thumb};

/// Geometry and drag state carried between frames.
#[derive(Default)]
pub struct ViewState {
    pub visible: Range<usize>,
    /// (index, rect) of rows drawn last frame, for drop targeting.
    pub rows: Vec<(usize, Rect)>,
    pub list_rect: Option<Rect>,
    pub drag: Option<Drag>,
    /// The column whose header is being dragged to a new position.
    header_drag: Option<Col>,
    pub art: ArtCache,
}

pub struct Drag {
    pub ids: HashSet<EntryId>,
    pub target: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Num,
    Art,
    Title,
    Artist,
    Album,
    Format,
    Bpm,
    Key,
    Time,
}

const COLUMN_IDS: [&str; 9] = ["num", "art", "title", "artist", "album", "format", "bpm", "key", "time"];

/// Space between columns, taken from the right of each one.
const GAP: f32 = 8.0;
/// Row height without and with the artwork column; the thumbnail fills the row.
const ROW_HEIGHT_ART: f32 = 24.0;

impl Col {
    const ALL: [Col; 9] = [Col::Num, Col::Art, Col::Title, Col::Artist, Col::Album, Col::Format, Col::Bpm, Col::Key, Col::Time];

    fn id(self) -> &'static str {
        COLUMN_IDS[Self::ALL.iter().position(|c| *c == self).unwrap_or(0)]
    }

    fn from_id(id: &str) -> Option<Col> {
        Self::ALL.iter().copied().find(|c| c.id() == id)
    }

    fn header(self) -> &'static str {
        match self {
            Col::Num => "#",
            Col::Art => "",
            Col::Title => "TITLE",
            Col::Artist => "ARTIST",
            Col::Album => "ALBUM",
            Col::Format => "FORMAT",
            Col::Bpm => "BPM",
            Col::Key => "KEY",
            Col::Time => "TIME",
        }
    }

    fn menu_label(self) -> &'static str {
        match self {
            Col::Num => "Number",
            Col::Art => "Artwork",
            Col::Title => "Title",
            Col::Artist => "Artist",
            Col::Album => "Album",
            Col::Format => "Format",
            Col::Bpm => "BPM",
            Col::Key => "Key",
            Col::Time => "Time",
        }
    }

    fn align(self) -> Align2 {
        if matches!(self, Col::Num | Col::Bpm | Col::Time) { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER }
    }
}

/// Saved column ids to columns: unknown and repeated ids dropped, never empty.
pub fn parse_columns(ids: &[String]) -> Vec<Col> {
    let mut out: Vec<Col> = Vec::new();
    for c in ids.iter().filter_map(|id| Col::from_id(id)) {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    if out.is_empty() { Col::ALL.to_vec() } else { out }
}

fn row_height(order: &[Col]) -> f32 {
    if order.contains(&Col::Art) { ROW_HEIGHT_ART } else { theme::ROW_HEIGHT }
}

/// Cell rects for the visible columns, left to right. Number, artwork, format, BPM, key and time are
/// fixed; title, artist and album share the rest by weight.
fn columns(order: &[Col], row: Rect) -> Vec<(Col, Rect)> {
    let fixed = |c: Col| match c {
        Col::Num => Some(46.0 + GAP),
        Col::Art => Some(ROW_HEIGHT_ART + GAP),
        Col::Format => Some(44.0 + GAP),
        Col::Bpm => Some(46.0 + GAP),
        Col::Key => Some(36.0 + GAP),
        Col::Time => Some(62.0 + GAP),
        _ => None,
    };
    let weight = |c: Col| match c {
        Col::Title => 0.44,
        _ => 0.28,
    };
    let fixed_total: f32 = order.iter().filter_map(|c| fixed(*c)).sum();
    let weight_total: f32 = order.iter().filter(|c| fixed(**c).is_none()).map(|c| weight(*c)).sum();
    let flex = (row.width() - fixed_total).max(60.0);
    let mut x = row.left();
    order
        .iter()
        .map(|&c| {
            let slot = fixed(c).unwrap_or_else(|| flex * weight(c) / weight_total);
            let rect = Rect::from_min_size(pos2(x, row.top()), vec2((slot - GAP).max(1.0), row.height()));
            x += slot;
            (c, rect)
        })
        .collect()
}

fn rect_of(cols: &[(Col, Rect)], c: Col) -> Option<Rect> {
    cols.iter().find(|(k, _)| *k == c).map(|(_, r)| *r)
}

/// Cover thumbnails for the artwork column, loaded lazily for the rows on screen.
#[derive(Default)]
pub struct ArtCache {
    /// Per file: the cover's content hash, `None` when it has no cover.
    by_path: HashMap<PathBuf, Option<u64>>,
    /// One texture per distinct image, shared by every track of an album.
    textures: HashMap<u64, TextureHandle>,
    inflight: HashSet<PathBuf>,
}

/// Textures kept before the cache is emptied (visible rows reload right away).
const MAX_TEXTURES: usize = 1000;

impl ArtCache {
    fn texture(&self, path: &Path) -> Option<&TextureHandle> {
        self.by_path.get(path).copied().flatten().and_then(|k| self.textures.get(&k))
    }

    /// The wanted paths to request now, when any of them is neither known nor on its way.
    fn to_request<'a>(&mut self, wanted: impl Iterator<Item = &'a Path>) -> Option<Vec<PathBuf>> {
        let missing: Vec<PathBuf> = wanted.filter(|p| !self.by_path.contains_key(*p)).map(Path::to_path_buf).collect();
        if missing.iter().all(|p| self.inflight.contains(p)) {
            return None;
        }
        self.inflight = missing.iter().cloned().collect();
        Some(missing)
    }

    pub fn insert(&mut self, ctx: &egui::Context, path: PathBuf, art: Option<(u64, Thumb)>) {
        self.inflight.remove(&path);
        let Some((key, thumb)) = art else {
            self.by_path.insert(path, None);
            return;
        };
        if !self.textures.contains_key(&key) {
            if self.textures.len() >= MAX_TEXTURES {
                self.textures.clear();
                self.by_path.clear();
            }
            let img = egui::ColorImage::from_rgba_unmultiplied(thumb.size, &thumb.rgba);
            // Mipmaps keep the 22 pt row thumbnail from shimmering when it is scrolled.
            let options = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
            self.textures.insert(key, ctx.load_texture(format!("cover-{key}"), img, options));
        }
        self.by_path.insert(path, Some(key));
    }

    /// Drop what is known about these files (their tags were rewritten).
    pub fn forget(&mut self, paths: &[PathBuf]) {
        for p in paths {
            self.by_path.remove(p);
        }
    }
}

/// Paint single-line text truncated with an ellipsis to the cell.
fn cell(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32, align: Align2) {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(rect.width().max(1.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let x = match align.x() {
        egui::Align::Max => rect.right() - galley.size().x,
        egui::Align::Center => rect.center().x - galley.size().x / 2.0,
        egui::Align::Min => rect.left(),
    };
    ui.painter().galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
}

pub fn show(app: &mut App, ui: &mut Ui) {
    egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
        ui.horizontal(|ui| {
            playlist_tabs(app, ui);
        });
    });
    egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| toolbar(app, ui));
    let order = parse_columns(&app.settings.playlist_columns);
    header(app, ui, &order);
    table(app, ui, &order);
}

fn playlist_tabs(app: &mut App, ui: &mut Ui) {
    egui::ScrollArea::horizontal().id_salt("pl-tabs").max_height(26.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut switch_to = None;
            for i in 0..app.player.lists.len() {
                let doc = &app.player.lists[i];
                let playing = app.player.current.as_ref().is_some_and(|c| c.list == i) && app.player.status != Status::Stopped;
                let label = if playing { format!("▶ {}", doc.playlist.name) } else { doc.playlist.name.clone() };
                let resp = widgets::tab(ui, i == app.player.viewing, &label)
                    .on_hover_text(format!("{} tracks · double-click to rename", doc.playlist.len()));
                if resp.clicked() {
                    switch_to = Some(i);
                }
                if resp.double_clicked() {
                    app.dialogs.rename = Some((i, doc.playlist.name.clone()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename…").clicked() {
                        app.dialogs.rename = Some((i, app.player.lists[i].playlist.name.clone()));
                        ui.close();
                    }
                    if ui.button("Export as M3U8…").clicked() {
                        dialogs::export_playlist(app, i);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Delete playlist…").clicked() {
                        app.dialogs.confirm_delete = Some(i);
                        ui.close();
                    }
                });
            }
            if ui.add(egui::Button::new("+").min_size(vec2(24.0, 22.0))).on_hover_text("New playlist").clicked() {
                app.dialogs.new_playlist = Some(String::new());
            }
            if let Some(i) = switch_to
                && i != app.player.viewing
            {
                app.player.viewing = i;
                app.selection = Default::default();
                app.pl_view.drag = None;
                app.mark_settings_dirty();
            }
        });
    });
}

fn toolbar(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if ui.button("Add files…").on_hover_text(widgets::cmd_key('O', false)).clicked() {
            dialogs::open_files(app);
        }
        if ui.button("Add folder…").on_hover_text(widgets::cmd_key('O', true)).clicked() {
            dialogs::open_folder(app);
        }
        ui.separator();
        let has_sel = !app.selection.ids.is_empty();
        if ui.add_enabled(has_sel, egui::Button::new("Remove")).on_hover_text("Delete / Backspace").clicked() {
            remove_selected(app);
        }
        if ui.add_enabled(has_sel, egui::Button::new("▲")).on_hover_text("Move up (⌥↑ / Alt+Up)").clicked() {
            move_selected(app, -1);
        }
        if ui.add_enabled(has_sel, egui::Button::new("▼")).on_hover_text("Move down (⌥↓ / Alt+Down)").clicked() {
            move_selected(app, 1);
        }
        ui.menu_button("Sort", |ui| {
            let list = app.player.viewing;
            let key = |e: &PlaylistEntry| e.info.display_title(&e.path).to_lowercase();
            if ui.button("By title").clicked() {
                app.player.edit_list(list, |p| p.sort_by(|a, b| key(a).cmp(&key(b))));
                ui.close();
            }
            if ui.button("By artist").clicked() {
                let artist = |e: &PlaylistEntry| e.info.artist.clone().unwrap_or_default().to_lowercase();
                app.player.edit_list(list, |p| p.sort_by(|a, b| artist(a).cmp(&artist(b)).then(key(a).cmp(&key(b)))));
                ui.close();
            }
            if ui.button("By album").clicked() {
                let album = |e: &PlaylistEntry| e.info.album.clone().unwrap_or_default().to_lowercase();
                app.player.edit_list(list, |p| p.sort_by(|a, b| album(a).cmp(&album(b)).then(a.path.cmp(&b.path))));
                ui.close();
            }
            if ui.button("By file path").clicked() {
                app.player.edit_list(list, |p| p.sort_by(|a, b| a.path.cmp(&b.path)));
                ui.close();
            }
            if ui.button("By BPM").clicked() {
                let bpm = |e: &PlaylistEntry| e.info.bpm.unwrap_or(f32::MAX);
                app.player.edit_list(list, |p| p.sort_by(|a, b| bpm(a).total_cmp(&bpm(b)).then(key(a).cmp(&key(b)))));
                ui.close();
            }
            if ui.button("By key").clicked() {
                let wheel = |e: &PlaylistEntry| e.info.key.map_or(u8::MAX, |k| k.wheel_order());
                app.player.edit_list(list, |p| p.sort_by(|a, b| wheel(a).cmp(&wheel(b)).then(key(a).cmp(&key(b)))));
                ui.close();
            }
            if ui.button("By duration").clicked() {
                let d = |e: &PlaylistEntry| e.info.duration.unwrap_or(f64::MAX);
                app.player.edit_list(list, |p| p.sort_by(|a, b| d(a).total_cmp(&d(b))));
                ui.close();
            }
            ui.separator();
            if ui.button("Reverse").clicked() {
                app.player.edit_list(list, |p| p.reverse());
                ui.close();
            }
        });
        ui.menu_button("List", |ui| {
            if ui.button("Import M3U / M3U8…").clicked() {
                dialogs::import_playlist(app);
                ui.close();
            }
            if ui.button("Export as M3U8…").clicked() {
                let i = app.player.viewing;
                dialogs::export_playlist(app, i);
                ui.close();
            }
            ui.separator();
            if ui.button("Select all").clicked() {
                select_all(app);
                ui.close();
            }
            if ui.button("Remove unavailable entries").clicked() {
                let list = app.player.viewing;
                let ids: HashSet<EntryId> = app.player.viewed().entries().iter().filter(|e| e.status == EntryStatus::Missing).map(|e| e.id).collect();
                let n = app.player.remove_entries(list, &ids);
                app.info(format!("Removed {n} unavailable entr{}", if n == 1 { "y" } else { "ies" }));
                ui.close();
            }
            if ui.button("Rescan tags").clicked() {
                let paths: Vec<PathBuf> = app.player.viewed().entries().iter().map(|e| e.path.clone()).collect();
                app.workers.scan(paths);
                ui.close();
            }
            ui.separator();
            if ui.button("Clear playlist…").clicked() {
                app.dialogs.confirm_clear = Some(app.player.viewing);
                ui.close();
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Drop the pill rather than let it overlap the Sort and List buttons in a narrow panel.
            const LABEL: &str = "◎ Now playing";
            let font = egui::TextStyle::Small.resolve(ui.style());
            let needed = ui.painter().layout_no_wrap(LABEL.into(), font, theme::text()).size().x + 2.0 * ui.spacing().button_padding.x;
            if app.player.current.is_some() && ui.available_width() >= needed && ui.small_button(LABEL).on_hover_text(format!("Scroll to the playing track ({})", widgets::cmd_key('J', false))).clicked() {
                let c = app.player.current.clone().unwrap();
                if app.player.viewing != c.list {
                    app.player.viewing = c.list;
                    app.selection = Default::default();
                }
                app.scroll_to = Some(c.entry);
            }
        });
    });
}

fn header(app: &mut App, ui: &mut Ui, order: &[Col]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::bg());
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, theme::border()));
    let cols = columns(order, rect.shrink2(vec2(8.0, 0.0)));
    let font = theme::prop(11.0);
    let dragging = app.pl_view.header_drag;
    let mut menu_change: Option<Vec<Col>> = None;
    for &(col, cell_rect) in &cols {
        // The whole slot is the drag handle and menu target, not just the label.
        let slot = Rect::from_min_max(pos2(cell_rect.left() - GAP / 2.0, rect.top()), pos2(cell_rect.right() + GAP / 2.0, rect.bottom()));
        let resp = ui.interact(slot, ui.make_persistent_id(("column-header", col.id())), Sense::click_and_drag());
        if resp.drag_started() {
            app.pl_view.header_drag = Some(col);
        }
        if dragging == Some(col) {
            ui.painter().rect_filled(slot, 0.0, theme::select_bg());
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if resp.hovered() && dragging.is_none() {
            ui.painter().rect_filled(slot, 0.0, theme::raised());
        }
        let color = if dragging == Some(col) { theme::text() } else { theme::text_faint() };
        cell(ui, cell_rect, col.header(), font.clone(), color, col.align());
        if dragging.is_none() {
            resp.clone().on_hover_text("Drag to reorder · right-click to show or hide columns");
        }
        resp.context_menu(|ui| {
            if let Some(next) = column_menu(ui, order) {
                menu_change = Some(next);
            }
        });
    }

    if let Some(dragged) = dragging {
        let others: Vec<&(Col, Rect)> = cols.iter().filter(|(c, _)| *c != dragged).collect();
        let pointer_x = ui.input(|i| i.pointer.latest_pos()).map(|p| p.x);
        let at = pointer_x.map_or(0, |x| others.iter().take_while(|(_, r)| r.center().x < x).count());
        let x = others.get(at).map(|(_, r)| r.left() - GAP / 2.0).or_else(|| others.last().map(|(_, r)| r.right() + GAP / 2.0));
        if let Some(x) = x {
            ui.painter().line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(2.0, theme::accent()));
        }
        if ui.input(|i| i.pointer.any_released()) {
            app.pl_view.header_drag = None;
            let mut next: Vec<Col> = order.iter().copied().filter(|c| *c != dragged).collect();
            next.insert(at.min(next.len()), dragged);
            menu_change = Some(next);
        }
    }
    if let Some(next) = menu_change.filter(|n| n != order) {
        set_columns(app, &next);
    }
}

/// Show/hide checkboxes for every column plus a reset; the new order when it changed.
fn column_menu(ui: &mut Ui, order: &[Col]) -> Option<Vec<Col>> {
    let mut next = None;
    for c in Col::ALL {
        let mut shown = order.contains(&c);
        let only = shown && order.len() == 1;
        if ui.add_enabled(!only, egui::Checkbox::new(&mut shown, c.menu_label())).clicked() {
            let mut v: Vec<Col> = order.to_vec();
            if shown {
                // Back where it sits by default, as far as the other columns allow.
                let home = Col::ALL.iter().position(|k| *k == c).unwrap_or(0);
                v.insert(home.min(v.len()), c);
            } else {
                v.retain(|k| *k != c);
            }
            next = Some(v);
        }
    }
    ui.separator();
    if ui.button("Reset columns").clicked() {
        next = Some(Col::ALL.to_vec());
        ui.close();
    }
    next
}

fn set_columns(app: &mut App, order: &[Col]) {
    app.settings.playlist_columns = order.iter().map(|c| c.id().to_owned()).collect();
    app.mark_settings_dirty();
}

fn table(app: &mut App, ui: &mut Ui, order: &[Col]) {
    let n = app.player.viewed().len();
    let row_h = row_height(order);
    let show_art = order.contains(&Col::Art);
    ui.spacing_mut().item_spacing.y = 0.0;
    let list_rect = ui.available_rect_before_wrap();
    app.pl_view.list_rect = Some(list_rect);

    if n == 0 {
        empty_state(app, ui, list_rect);
        return;
    }

    let mut area = egui::ScrollArea::vertical().id_salt(("pl", app.player.viewing)).auto_shrink([false, false]);
    if let Some(target) = app.scroll_to.take()
        && let Some(i) = app.player.viewed().index_of(target)
        && !(app.pl_view.visible.start + 1..app.pl_view.visible.end.saturating_sub(1)).contains(&i)
    {
        let offset = (i as f32 * row_h - list_rect.height() / 2.0 + row_h).max(0.0);
        area = area.vertical_scroll_offset(offset);
    }

    let pointer = ui.input(|i| i.pointer.interact_pos());
    let released = ui.input(|i| i.pointer.any_released());
    let mut rows = Vec::new();
    let mut play: Option<EntryId> = None;

    let output = area.show_rows(ui, row_h, n, |ui, range| {
        app.pl_view.visible = range.clone();
        if show_art {
            // A few rows beyond the edge so covers are there before they scroll in.
            let ahead = range.start..(range.end + 12).min(n);
            let list = app.player.viewed().entries();
            let wanted = list[ahead].iter().filter(|e| e.status.is_playable()).map(|e| e.path.as_path());
            if let Some(paths) = app.pl_view.art.to_request(wanted) {
                app.workers.request_covers(paths);
            }
        }
        for i in range {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
            rows.push((i, rect));
            // A context menu action can remove entries mid-loop; stop at the new end.
            let Some(entry) = app.player.viewed().entries().get(i).cloned() else { break };
            let id = entry.id;
            paint_row(app, ui, rect, order, i, &entry, resp.hovered());

            if resp.clicked() {
                click_row(app, ui, id);
            }
            if resp.double_clicked() {
                play = Some(id);
            }
            if resp.secondary_clicked() && !app.selection.ids.contains(&id) {
                set_single(app, id);
            }
            if resp.drag_started() {
                if !app.selection.ids.contains(&id) {
                    set_single(app, id);
                }
                app.pl_view.drag = Some(Drag { ids: app.selection.ids.clone(), target: None });
            }
            let tooltip = row_tooltip(&entry);
            // Over the artwork cell the tooltip leads with a larger picture.
            let over_art = rect_of(&columns(order, rect.shrink2(vec2(8.0, 0.0))), Col::Art).zip(pointer).is_some_and(|(r, p)| p.x >= r.left() && p.x <= r.right());
            let preview = over_art.then(|| app.pl_view.art.texture(&entry.path).map(|t| (t.id(), t.size_vec2()))).flatten();
            let resp = resp.on_hover_ui_at_pointer(|ui| {
                if let Some((id, size)) = preview {
                    ui.add(egui::Image::new((id, size)).corner_radius(2.0));
                }
                ui.label(tooltip);
            });
            resp.context_menu(|ui| row_menu(app, ui, id));
        }
    });

    // Drag-to-reorder: insertion point from the pointer position.
    if let Some(drag) = app.pl_view.drag.as_mut() {
        if let Some(p) = pointer {
            drag.target = insertion_index(&rows, p.y, n);
            // Auto-scroll near the edges.
            let r = output.inner_rect;
            let edge = 24.0;
            if p.y < r.top() + edge {
                ui.scroll_with_delta(vec2(0.0, 8.0));
            } else if p.y > r.bottom() - edge {
                ui.scroll_with_delta(vec2(0.0, -8.0));
            }
            if let Some(t) = drag.target {
                let y = rows.iter().find(|(i, _)| *i == t).map(|(_, r)| r.top()).or_else(|| rows.last().map(|(_, r)| r.bottom()));
                if let Some(y) = y {
                    let x = output.inner_rect.x_range();
                    ui.painter().line_segment([pos2(x.min + 4.0, y), pos2(x.max - 4.0, y)], Stroke::new(2.0, theme::accent()));
                }
            }
        }
        if released {
            let drag = app.pl_view.drag.take().unwrap();
            if let Some(t) = drag.target {
                let list = app.player.viewing;
                app.player.edit_list(list, |p| {
                    p.move_to(&drag.ids, t);
                });
            }
        }
    }

    // External files hovering: show where they will land.
    let hovering_files = ui.input(|i| !i.raw.hovered_files.is_empty());
    if hovering_files {
        let t = ui.input(|i| i.pointer.hover_pos()).and_then(|p| drop_slot(list_rect, &rows, p, n)).unwrap_or(n);
        let y = rows.iter().find(|(i, _)| *i == t).map(|(_, r)| r.top()).or_else(|| rows.last().map(|(_, r)| r.bottom()));
        if let Some(y) = y {
            ui.painter().line_segment([pos2(list_rect.left() + 4.0, y), pos2(list_rect.right() - 4.0, y)], Stroke::new(2.0, theme::ok()));
        }
    }
    app.pl_view.rows = rows;

    if let Some(id) = play {
        let list = app.player.viewing;
        app.player.play_entry(list, id, 0.0);
    }
}

fn insertion_index(rows: &[(usize, Rect)], y: f32, n: usize) -> Option<usize> {
    let (first, last) = (rows.first()?, rows.last()?);
    if y < first.1.top() {
        return Some(first.0);
    }
    if y > last.1.bottom() {
        return Some((last.0 + 1).min(n));
    }
    rows.iter().find(|(_, r)| y >= r.top() && y < r.bottom()).map(|(i, r)| if y < r.center().y { *i } else { i + 1 })
}

/// The gap under the pointer, or `None` outside the list (files then go to the end).
fn drop_slot(list_rect: Rect, rows: &[(usize, Rect)], p: Pos2, n: usize) -> Option<usize> {
    if !list_rect.contains(p) {
        return None;
    }
    insertion_index(rows, p.y, n)
}

/// Where dropped files go: the gap under the pointer, or the end.
pub fn drop_index(app: &App, ctx: &egui::Context) -> Option<usize> {
    let p = ctx.input(|i| i.pointer.hover_pos())?;
    if !app.playlist_open {
        return None;
    }
    drop_slot(app.pl_view.list_rect?, &app.pl_view.rows, p, app.player.viewed().len())
}

fn paint_row(app: &App, ui: &Ui, rect: Rect, order: &[Col], i: usize, e: &PlaylistEntry, hovered: bool) {
    let p = ui.painter();
    let selected = app.selection.ids.contains(&e.id);
    let current = app.player.is_current(app.player.viewing, e.id);
    let bg = if selected {
        theme::select_bg()
    } else if hovered {
        theme::raised()
    } else if i % 2 == 1 {
        theme::row_alt()
    } else {
        theme::panel()
    };
    p.rect_filled(rect, 0.0, bg);
    if app.selection.cursor == Some(e.id) && selected {
        p.rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0, theme::select_stroke()), egui::StrokeKind::Inside);
    }
    if current {
        p.rect_filled(Rect::from_min_size(rect.left_top(), vec2(3.0, rect.height())), 0.0, theme::accent());
    }
    let cols = columns(order, rect.shrink2(vec2(8.0, 0.0)));
    let playable = e.status.is_playable();
    let (fg, dim) = match (current, playable) {
        (true, _) => (theme::accent(), theme::accent().gamma_multiply(0.8)),
        (false, false) => (theme::text_faint(), theme::text_faint()),
        _ => (theme::text(), theme::text_dim()),
    };
    let font = theme::prop(12.5);
    for &(col, r) in &cols {
        match col {
            Col::Num if current => {
                let icon = match app.player.status {
                    Status::Paused => Icon::Pause,
                    Status::Stopped => Icon::Stop,
                    _ => Icon::Play,
                };
                // At the left of the cell so the track number stays readable beside it.
                let icon_rect = Rect::from_center_size(pos2(r.left() + 5.0, r.center().y), vec2(10.0, 10.0));
                widgets::paint_icon(p, icon_rect, icon, theme::accent());
                cell(ui, r, &format!("{}", i + 1), theme::mono(11.0), theme::accent().gamma_multiply(0.8), col.align());
            }
            Col::Num if !playable => cell(ui, r, "⚠", font.clone(), theme::warn(), col.align()),
            Col::Num => cell(ui, r, &format!("{}", i + 1), theme::mono(11.0), theme::text_faint(), col.align()),
            Col::Art => paint_cover(p, r, app.pl_view.art.texture(&e.path)),
            Col::Title => cell(ui, r, &e.info.display_title(&e.path), font.clone(), fg, col.align()),
            Col::Artist => cell(ui, r, e.info.artist.as_deref().unwrap_or(""), font.clone(), fg, col.align()),
            Col::Album => cell(ui, r, e.info.album.as_deref().unwrap_or(""), font.clone(), dim, col.align()),
            Col::Format => cell(ui, r, &format_label(&e.path), theme::mono(10.5), if current { dim } else { theme::text_faint() }, col.align()),
            Col::Bpm => {
                // A "~" marks a tempo found by listening rather than read from the tags.
                let text = e.info.bpm.map(|b| format!("{}{}", if e.info.bpm_detected { "~" } else { "" }, super::format_bpm(b))).unwrap_or_default();
                cell(ui, r, &text, theme::mono(11.5), dim, col.align());
            }
            Col::Key => {
                if let Some(k) = e.info.key {
                    cell(ui, r, &k.display(app.key_notation()), theme::mono(11.5), theme::camelot_color(k.camelot_number()), col.align());
                }
            }
            Col::Time => {
                let time = e.info.duration.map(nullamp_core::time::format_clock).unwrap_or_default();
                cell(ui, r, &time, theme::mono(11.5), dim, col.align());
            }
        }
    }
}

/// The container as the file extension names it, upper-cased (`MP3`, `FLAC`, `AIFF`).
fn format_label(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
}

/// A square thumbnail, centre-cropped to fill; a faint tile while there is none.
fn paint_cover(p: &egui::Painter, cell: Rect, tex: Option<&TextureHandle>) {
    let square = Rect::from_center_size(cell.center(), Vec2::splat(ROW_HEIGHT_ART - 2.0));
    let Some(tex) = tex else {
        p.rect_filled(square, 2.0, theme::raised());
        return;
    };
    let size = tex.size_vec2();
    let side = size.x.min(size.y);
    let crop = vec2((size.x - side) / size.x / 2.0, (size.y - side) / size.y / 2.0);
    let uv = Rect::from_min_max(pos2(crop.x, crop.y), pos2(1.0 - crop.x, 1.0 - crop.y));
    p.add(egui::epaint::RectShape::filled(square, 2.0, Color32::WHITE).with_texture(tex.id(), uv));
}

fn row_tooltip(e: &PlaylistEntry) -> String {
    let status = match &e.status {
        EntryStatus::Missing => "\n⚠ File not found (moved, deleted, or on a disconnected drive)".to_owned(),
        EntryStatus::Unsupported(why) => format!("\n⚠ Cannot play: {why}"),
        _ => String::new(),
    };
    format!("{}{status}", e.path.display())
}

fn row_menu(app: &mut App, ui: &mut Ui, id: EntryId) {
    let list = app.player.viewing;
    if ui.button("▶ Play").clicked() {
        app.player.play_entry(list, id, 0.0);
        ui.close();
    }
    if ui.button("Inspect metadata").clicked() {
        app.side_open = true;
        app.side_tab = SideTab::Inspector;
        inspector::load_from_selection(app);
        ui.close();
    }
    if ui.button("Log fingerprint").on_hover_text("Chromaprint of the first two minutes, written to the log").clicked() {
        if let Some(e) = app.player.viewed().get(id) {
            app.workers.submit(Job::LogFingerprint { path: e.path.clone() });
        }
        ui.close();
    }
    if ui.button("Show in folder").clicked() {
        if let Some(e) = app.player.viewed().get(id) {
            dialogs::reveal(&e.path);
        }
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        if let Some(e) = app.player.viewed().get(id) {
            ui.ctx().copy_text(e.path.display().to_string());
        }
        ui.close();
    }
    ui.separator();
    if ui.button("Move up").clicked() {
        move_selected(app, -1);
        ui.close();
    }
    if ui.button("Move down").clicked() {
        move_selected(app, 1);
        ui.close();
    }
    if ui.button(format!("Remove ({})", app.selection.ids.len().max(1))).clicked() {
        remove_selected(app);
        ui.close();
    }
}

fn empty_state(app: &mut App, ui: &mut Ui, rect: Rect) {
    let resp = ui.allocate_rect(rect, Sense::click());
    let p = ui.painter();
    let c = rect.center();
    p.text(c - vec2(0.0, 14.0), Align2::CENTER_CENTER, "This playlist is empty", theme::prop(15.0), theme::text_dim());
    p.text(
        c + vec2(0.0, 10.0),
        Align2::CENTER_CENTER,
        "Drop files or folders here, or click to add files",
        theme::prop(12.5),
        theme::text_faint(),
    );
    if resp.clicked() {
        dialogs::open_files(app);
    }
}

// ----- Selection and editing -----

fn set_single(app: &mut App, id: EntryId) {
    app.selection.ids = HashSet::from([id]);
    app.selection.anchor = Some(id);
    app.selection.cursor = Some(id);
}

fn click_row(app: &mut App, ui: &Ui, id: EntryId) {
    let m = ui.input(|i| i.modifiers);
    if m.shift {
        let anchor = app.selection.anchor.unwrap_or(id);
        let range = app.player.viewed().range(anchor, id);
        if !m.command {
            app.selection.ids.clear();
        }
        app.selection.ids.extend(range);
        app.selection.cursor = Some(id);
    } else if m.command {
        if !app.selection.ids.remove(&id) {
            app.selection.ids.insert(id);
        }
        app.selection.anchor = Some(id);
        app.selection.cursor = Some(id);
    } else {
        set_single(app, id);
    }
    if app.side_open && app.shown_tab() == SideTab::Inspector && !app.settings.inspector_follows_playing {
        inspector::load_from_selection(app);
    }
}

fn select_all(app: &mut App) {
    app.selection.ids = app.player.viewed().ids().collect();
}

fn remove_selected(app: &mut App) {
    if app.selection.ids.is_empty() {
        return;
    }
    let list = app.player.viewing;
    // Keep the cursor near where the removed block was.
    let next = {
        let pl = app.player.viewed();
        let last = pl.entries().iter().rposition(|e| app.selection.ids.contains(&e.id));
        last.and_then(|i| pl.entries()[i + 1..].iter().find(|e| !app.selection.ids.contains(&e.id)).map(|e| e.id))
            .or_else(|| pl.entries().iter().rev().find(|e| !app.selection.ids.contains(&e.id)).map(|e| e.id))
    };
    let ids = std::mem::take(&mut app.selection.ids);
    let n = app.player.remove_entries(list, &ids);
    if let Some(id) = next {
        set_single(app, id);
    }
    app.info(format!("Removed {n} track{}", if n == 1 { "" } else { "s" }));
}

fn move_selected(app: &mut App, delta: isize) {
    let list = app.player.viewing;
    let ids = app.selection.ids.clone();
    app.player.edit_list(list, |p| {
        p.move_by(&ids, delta);
    });
    app.scroll_to = app.selection.cursor;
}

/// Playlist keys: arrows, Enter, Delete, ⌘A, ⌥↑/⌥↓, Home/End.
pub fn handle_keys(app: &mut App, ctx: &egui::Context) {
    let n = app.player.viewed().len();
    if n == 0 {
        return;
    }
    let (up, down, enter, delete, all, home, end, shift, alt) = ctx.input(|i| {
        (
            i.key_pressed(Key::ArrowUp),
            i.key_pressed(Key::ArrowDown),
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
            i.modifiers.command && i.key_pressed(Key::A),
            i.key_pressed(Key::Home),
            i.key_pressed(Key::End),
            i.modifiers.shift,
            i.modifiers.alt,
        )
    });
    if all {
        select_all(app);
        return;
    }
    if alt && (up || down) {
        move_selected(app, if up { -1 } else { 1 });
        return;
    }
    if delete {
        remove_selected(app);
        return;
    }
    if enter
        && let Some(id) = app.selection.cursor.filter(|id| app.player.viewed().get(*id).is_some())
    {
        let list = app.player.viewing;
        app.player.play_entry(list, id, 0.0);
        return;
    }
    let cur = app.selection.cursor.and_then(|id| app.player.viewed().index_of(id));
    let target = if up {
        Some(cur.map(|i| i.saturating_sub(1)).unwrap_or(0))
    } else if down {
        Some(cur.map(|i| (i + 1).min(n - 1)).unwrap_or(0))
    } else if home {
        Some(0)
    } else if end {
        Some(n - 1)
    } else {
        None
    };
    if let Some(t) = target {
        let id = app.player.viewed().entries()[t].id;
        if shift {
            let anchor = app.selection.anchor.unwrap_or(id);
            app.selection.ids = app.player.viewed().range(anchor, id).into_iter().collect();
            app.selection.cursor = Some(id);
        } else {
            set_single(app, id);
        }
        app.scroll_to = Some(id);
    }
}

/// Create a playlist and switch to it.
pub fn new_playlist(app: &mut App, name: &str) {
    let name = if name.trim().is_empty() { "Untitled".to_owned() } else { name.trim().to_owned() };
    let taken: HashSet<String> = app.player.lists.iter().map(|d| d.key.clone()).collect();
    let key = app.store.new_playlist_key(&name, &taken);
    let i = app.player.add_list(PlaylistDoc { key, playlist: nullamp_core::Playlist::new(name) });
    app.player.viewing = i;
    app.selection = Default::default();
    app.mark_list_dirty(i);
}
