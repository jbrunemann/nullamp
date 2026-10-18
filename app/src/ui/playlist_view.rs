//! The playlist editor: named playlists, a dense virtualized table, selection,
//! drag-to-reorder and keyboard editing.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, FontId, Key, Rect, Sense, Stroke, Ui, pos2, vec2};
use nullamp_core::{EntryId, EntryStatus, PlaylistEntry};

use super::widgets::{self, Icon};
use super::{App, SideTab, dialogs, inspector, theme};
use crate::player::Status;
use crate::store::PlaylistDoc;

/// Geometry and drag state carried between frames.
#[derive(Default)]
pub struct ViewState {
    pub visible: Range<usize>,
    /// (index, rect) of rows drawn last frame, for drop targeting.
    pub rows: Vec<(usize, Rect)>,
    pub list_rect: Option<Rect>,
    pub drag: Option<Drag>,
}

pub struct Drag {
    pub ids: HashSet<EntryId>,
    pub target: Option<usize>,
}

struct Columns {
    num: Rect,
    title: Rect,
    artist: Rect,
    album: Rect,
    time: Rect,
}

fn columns(row: Rect) -> Columns {
    let num_w = 46.0;
    let time_w = 62.0;
    let flex = (row.width() - num_w - time_w - 16.0).max(60.0);
    let (tw, aw) = (flex * 0.44, flex * 0.28);
    let x0 = row.left();
    let col = |x: f32, w: f32| Rect::from_min_size(pos2(x, row.top()), vec2(w, row.height()));
    Columns {
        num: col(x0, num_w),
        title: col(x0 + num_w + 4.0, tw - 8.0),
        artist: col(x0 + num_w + tw, aw - 8.0),
        album: col(x0 + num_w + tw + aw, flex - tw - aw - 8.0),
        time: col(row.right() - time_w - 8.0, time_w),
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
    header(ui);
    table(app, ui);
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
        if ui.button("Add files…").on_hover_text("⌘O / Ctrl+O").clicked() {
            dialogs::open_files(app);
        }
        if ui.button("Add folder…").on_hover_text("⇧⌘O / Ctrl+Shift+O").clicked() {
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
            if app.player.current.is_some() && ui.small_button("◎ Now playing").on_hover_text("Scroll to the playing track (⌘J / Ctrl+J)").clicked() {
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

fn header(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::bg());
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, theme::border()));
    let cols = columns(rect.shrink2(vec2(8.0, 0.0)));
    let font = theme::prop(11.0);
    let c = theme::text_faint();
    cell(ui, cols.num, "#", font.clone(), c, Align2::RIGHT_CENTER);
    cell(ui, cols.title, "TITLE", font.clone(), c, Align2::LEFT_CENTER);
    cell(ui, cols.artist, "ARTIST", font.clone(), c, Align2::LEFT_CENTER);
    cell(ui, cols.album, "ALBUM", font.clone(), c, Align2::LEFT_CENTER);
    cell(ui, cols.time, "TIME", font, c, Align2::RIGHT_CENTER);
}

fn table(app: &mut App, ui: &mut Ui) {
    let n = app.player.viewed().len();
    let row_h = theme::ROW_HEIGHT;
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
        for i in range {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
            rows.push((i, rect));
            let entry = app.player.viewed().entries()[i].clone();
            let id = entry.id;
            paint_row(app, ui, rect, i, &entry, resp.hovered());

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
            let resp = resp.on_hover_text_at_pointer(tooltip);
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
    if hovering_files
        && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        && let Some(t) = insertion_index(&rows, p.y, n)
    {
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

/// Where dropped files go: the gap under the pointer, or the end.
pub fn drop_index(app: &App, ctx: &egui::Context) -> Option<usize> {
    let p = ctx.input(|i| i.pointer.hover_pos())?;
    if !app.playlist_open || !app.pl_view.list_rect.is_some_and(|r| r.contains(p)) {
        return None;
    }
    insertion_index(&app.pl_view.rows, p.y, app.player.viewed().len())
}

fn paint_row(app: &App, ui: &Ui, rect: Rect, i: usize, e: &PlaylistEntry, hovered: bool) {
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
    let cols = columns(rect.shrink2(vec2(8.0, 0.0)));
    let playable = e.status.is_playable();
    let (fg, dim) = match (current, playable) {
        (true, _) => (theme::accent(), theme::accent().gamma_multiply(0.8)),
        (false, false) => (theme::text_faint(), theme::text_faint()),
        _ => (theme::text(), theme::text_dim()),
    };
    let font = theme::prop(12.5);
    if current {
        let icon = match app.player.status {
            Status::Paused => Icon::Pause,
            Status::Stopped => Icon::Stop,
            _ => Icon::Play,
        };
        let r = Rect::from_center_size(pos2(cols.num.right() - 7.0, cols.num.center().y), vec2(10.0, 10.0));
        widgets::paint_icon(p, r, icon, theme::accent());
    } else if !playable {
        cell(ui, cols.num, "⚠", font.clone(), theme::warn(), Align2::RIGHT_CENTER);
    } else {
        cell(ui, cols.num, &format!("{}", i + 1), theme::mono(11.0), theme::text_faint(), Align2::RIGHT_CENTER);
    }
    cell(ui, cols.title, &e.info.display_title(&e.path), font.clone(), fg, Align2::LEFT_CENTER);
    cell(ui, cols.artist, e.info.artist.as_deref().unwrap_or(""), font.clone(), dim, Align2::LEFT_CENTER);
    cell(ui, cols.album, e.info.album.as_deref().unwrap_or(""), font, dim, Align2::LEFT_CENTER);
    let time = e.info.duration.map(nullamp_core::time::format_clock).unwrap_or_default();
    cell(ui, cols.time, &time, theme::mono(11.5), dim, Align2::RIGHT_CENTER);
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
    if app.side_open && app.side_tab == SideTab::Inspector && !app.inspector.follow_playing {
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
