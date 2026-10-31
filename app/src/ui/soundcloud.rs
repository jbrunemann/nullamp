//! "Download from SoundCloud": a public track or set, reassembled from HLS into one `.m4a`.
//!
//! The first time, the dialog spells out that this bypasses SoundCloud's stream-only policy and
//! uses a client key scraped from the public web player; the user says they understand once and
//! the setting is kept so the next download skips the long explanation. The download itself runs on
//! the SoundCloud worker thread (see `workers.rs`); progress ticks come back as job results and
//! update this dialog, and the finished file is added to the playlist that was open when it started.

use egui::RichText;

use super::{App, theme, widgets};

/// The dialog, from the URL being typed through the download to its end.
#[derive(Default)]
pub struct Download {
    pub url: String,
    pub error: Option<String>,
    /// The running request. `None` before Download is clicked and after the job ends.
    pub request: Option<u64>,
    pub progress: Progress,
    /// Artist and title, shown next to the progress bar once the track resolves.
    pub label: Option<String>,
    /// The list the finished file is added to (recorded when Download is pressed, so the user
    /// switching playlists mid-download does not drop the result into the wrong one).
    pub target_list: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub step: Option<&'static str>,
    pub segments_total: usize,
    pub segments_done: usize,
    pub bytes: u64,
}

/// What the worker sends across the channel. Mirrors `nullamp_soundcloud::Step` but owns its
/// strings so it can cross threads.
pub enum Update {
    LookingUpClientId,
    Resolving,
    Streaming { title: String, artist: String, segments: usize },
    Segment { done: usize, total: usize, bytes: u64 },
}

/// What the worker sends when a download succeeds.
pub struct Done {
    pub path: std::path::PathBuf,
    pub artist: String,
    pub title: String,
}

pub fn open(app: &mut App) {
    app.dialogs.soundcloud = Some(Download { target_list: app.player.viewing, ..Default::default() });
}

pub fn modal(app: &mut App, ctx: &egui::Context, enter: bool, escape: bool) {
    let Some(mut dl) = app.dialogs.soundcloud.take() else { return };
    let (mut keep, mut run) = (true, false);
    let accepted = app.settings.soundcloud_accepted;
    let busy = dl.request.is_some();
    egui::Modal::new(egui::Id::unique("download-soundcloud")).show(ctx, |ui| {
        ui.set_width(widgets::modal_width(ctx, 520.0));
        ui.heading("Download from SoundCloud");
        if !accepted {
            ui.label(
                RichText::new(
                    "SoundCloud lets a browser stream a set, not save it. Nullamp reads the segments of the stream and joins them into a file; this is against SoundCloud's terms of service, and the key the web player uses may stop working at any time. The audio is passed through unchanged.",
                )
                .small()
                .color(theme::text_dim()),
            );
            ui.add_space(4.0);
        }
        ui.label(RichText::new("Paste the link of a track or set.").small().color(theme::text_dim()));
        let field = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut dl.url).hint_text("https://soundcloud.com/… or https://on.soundcloud.com/…").desired_width(f32::INFINITY));
        if !busy {
            field.request_focus();
        }
        if let Some(error) = &dl.error {
            ui.label(RichText::new(error).small().color(theme::err()));
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(progress_line(&dl.progress, dl.label.as_deref())).small().color(theme::text_dim()));
            });
            if dl.progress.segments_total > 0 {
                let fraction = (dl.progress.segments_done as f32 / dl.progress.segments_total as f32).clamp(0.0, 1.0);
                ui.add(egui::ProgressBar::new(fraction));
            }
        }
        ui.horizontal(|ui| {
            let ready = !busy && !dl.url.trim().is_empty();
            let label = if accepted { "Download" } else { "I understand, download" };
            if ui.add_enabled(ready, egui::Button::new(label)).clicked() || (ready && enter) {
                run = true;
            }
            let close_label = if busy { "Hide" } else { "Cancel" };
            if ui.button(close_label).clicked() || escape {
                keep = false;
            }
        });
        if busy {
            ui.label(RichText::new("The download keeps running when you close this dialog.").small().color(theme::text_dim()));
        }
    });
    if run {
        let request = app.workers.request_id();
        dl.request = Some(request);
        dl.error = None;
        dl.progress = Progress::default();
        dl.label = None;
        dl.target_list = app.player.viewing;
        if !app.settings.soundcloud_accepted {
            app.settings.soundcloud_accepted = true;
            app.mark_settings_dirty();
        }
        app.workers.fetch_soundcloud(request, dl.url.trim().to_owned(), dl.target_list);
    }
    if keep {
        app.dialogs.soundcloud = Some(dl);
    }
}

fn progress_line(p: &Progress, label: Option<&str>) -> String {
    let step = p.step.unwrap_or("Preparing download…");
    if let Some(name) = label {
        if p.segments_total > 0 {
            return format!("{name}: {} of {} ({:.1} MB)", p.segments_done, p.segments_total, p.bytes as f64 / (1 << 20) as f64);
        }
        return format!("{step} ({name})");
    }
    step.into()
}

/// A progress tick from the worker.
pub fn on_progress(app: &mut App, request: u64, update: Update) {
    let Some(dl) = app.dialogs.soundcloud.as_mut().filter(|d| d.request == Some(request)) else { return };
    match update {
        Update::LookingUpClientId => dl.progress.step = Some("Looking up SoundCloud key…"),
        Update::Resolving => dl.progress.step = Some("Resolving track…"),
        Update::Streaming { title, artist, segments } => {
            dl.label = Some(if artist.is_empty() { title } else { format!("{artist} - {title}") });
            dl.progress.step = Some("Downloading…");
            dl.progress.segments_total = segments;
            dl.progress.segments_done = 0;
            dl.progress.bytes = 0;
        }
        Update::Segment { done, total, bytes } => {
            dl.progress.segments_done = done;
            dl.progress.segments_total = total;
            dl.progress.bytes = bytes;
        }
    }
}

/// A finished (or failed) download. The dialog is closed on success and left open with the error on
/// failure, so the user can edit the URL and try again. `target_list` is the list the file goes
/// into; if the dialog was closed while the download ran, the file is still added.
pub fn on_done(app: &mut App, request: u64, target_list: usize, result: Result<Done, String>) {
    let dialog_matches = app.dialogs.soundcloud.as_ref().is_some_and(|d| d.request == Some(request));
    match result {
        Ok(done) => {
            if dialog_matches {
                app.dialogs.soundcloud = None;
            }
            let name = if done.artist.is_empty() { done.title.clone() } else { format!("{} - {}", done.artist, done.title) };
            let text = if name.is_empty() { format!("Downloaded {}", done.path.display()) } else { format!("Downloaded {name}") };
            if target_list < app.player.lists.len() {
                app.add_paths(target_list, vec![done.path], None);
            }
            app.info(text);
        }
        Err(e) => {
            if dialog_matches {
                let d = app.dialogs.soundcloud.as_mut().unwrap();
                d.request = None;
                d.error = Some(e);
            } else {
                app.error(format!("SoundCloud download failed: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_falls_back_to_the_step_until_the_track_resolves() {
        let p = Progress { step: Some("Resolving track…"), ..Progress::default() };
        assert_eq!(progress_line(&p, None), "Resolving track…");
    }

    #[test]
    fn progress_line_shows_segment_counts_and_bytes_once_a_label_is_known() {
        let p = Progress { step: Some("Downloading…"), segments_total: 919, segments_done: 100, bytes: 20 * (1 << 20) };
        assert_eq!(progress_line(&p, Some("CHRIS STASSY - Set")), "CHRIS STASSY - Set: 100 of 919 (20.0 MB)");
    }
}
