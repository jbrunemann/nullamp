//! Asking a Telegram bot for a track of a mix, and the Settings tab where the user signs in.
//!
//! The question goes out as the user's own account (`nullamp-telegram`). Nothing contacts Telegram
//! until the user signs in on the tab, asks for a track, or opens the tab with a saved login.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::RichText;
use nullamp_core::{EntryId, PlaylistEntry};
use nullamp_telegram::{Ask, Choice, Credentials, Event, LoginStep, Outcome, Progress};

use super::{App, dialogs, playlist_view, theme, widgets};
use crate::paths::AppPaths;
use crate::store::Settings;

/// How long to wait for the bot's sound file after sending the question.
pub const WAIT: Duration = Duration::from_secs(30);

/// The playlist the files the bot sends are added to.
const LIST: &str = "Telegram";

/// Who is signed in, as far as we know. Asked once per run, when the tab is shown.
#[derive(Debug, PartialEq)]
pub enum Account {
    Unknown,
    Checking,
    SignedOut,
    SignedIn(String),
}

/// Where the sign-in is.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    Idle,
    /// Waiting for Telegram: what for.
    Working(&'static str),
    /// A code has been sent to the user's Telegram apps.
    Code,
    /// The account has two-step verification; the password hint, if it has one.
    Password(Option<String>),
}

/// A question to the bot that is running.
pub struct Asking {
    pub(crate) request: u64,
    /// The entry of the setlist it is about.
    pub entry: EntryId,
    /// What was asked, "Artist - Title".
    pub(crate) query: String,
}

/// The results the bot offered, while the user chooses.
pub struct Choosing {
    request: u64,
    /// The entry of the setlist the query is about.
    entry: EntryId,
    /// What was asked for these results.
    query: String,
    /// The query as the user is editing it, to ask again when the results are not good.
    pub(crate) edited: String,
    choices: Vec<Choice>,
    /// The pictures that have arrived, by the index of the result.
    pub(crate) pictures: HashMap<usize, egui::TextureHandle>,
}

pub struct State {
    pub account: Account,
    pub(crate) step: Step,
    code: String,
    password: String,
    pub(crate) error: Option<String>,
    /// The password hint Telegram gave, kept for a second try.
    hint: Option<String>,
    /// A login is saved, so asking may work without the tab ever being opened. Looked up once at
    /// start and kept up to date by the sign-in events, so no frame touches the disk.
    session_saved: bool,
    /// The saved login has been checked this run.
    checked: bool,
    pub asking: Option<Asking>,
    pub choosing: Option<Choosing>,
}

impl State {
    pub fn new(paths: &AppPaths) -> State {
        State {
            account: Account::Unknown,
            step: Step::Idle,
            code: String::new(),
            password: String::new(),
            error: None,
            hint: None,
            session_saved: nullamp_telegram::Telegram::has_session(&paths.telegram_session()),
            checked: false,
            asking: None,
            choosing: None,
        }
    }
}

/// The folder the bot's files go to unless the user chose one: `Nullamp Telegram` in the Music folder.
/// Under `NULLAMP_HOME` (tests, portable use) it is inside that folder instead, so nothing lands in
/// the user's own Music folder.
pub fn default_dir(paths: &AppPaths) -> PathBuf {
    if std::env::var_os("NULLAMP_HOME").is_some() {
        return paths.data.join("telegram");
    }
    directories::UserDirs::new().and_then(|d| d.audio_dir().map(Path::to_path_buf)).unwrap_or_else(|| paths.data.clone()).join("Nullamp Telegram")
}

fn download_dir(app: &App) -> PathBuf {
    app.settings.telegram_dir.clone().unwrap_or_else(|| default_dir(&app.store.paths))
}

fn credentials(settings: &Settings) -> Option<Credentials> {
    let api_id = settings.telegram_api_id.trim().parse().ok()?;
    let api_hash = settings.telegram_api_hash.trim();
    (!api_hash.is_empty()).then(|| Credentials { api_id, api_hash: api_hash.to_owned() })
}

fn bot(settings: &Settings) -> &str {
    settings.telegram_bot.trim().trim_start_matches('@')
}

/// Why a track cannot be asked for now, `None` when it can: nothing is set up, or a question is running.
pub fn unavailable(app: &App) -> Option<&'static str> {
    if credentials(&app.settings).is_none() || bot(&app.settings).is_empty() {
        Some("Set up Telegram first: Settings > Telegram")
    } else if app.telegram.account == Account::SignedOut || (app.telegram.account == Account::Unknown && !app.telegram.session_saved) {
        Some("Sign in to Telegram first: Settings > Telegram")
    } else {
        None
    }
}

/// Ask the bot for "Artist - Title" of the entry, as typing "@bot Artist - Title" in a chat does, and
/// wait [`WAIT`] for a sound file.
pub fn ask_entry(app: &mut App, entry: EntryId) {
    let Some(text) = app.mix.setlist.as_ref().and_then(|s| s.entries.iter().find(|e| e.id == entry)).filter(|e| !e.is_unidentified()).map(|e| format!("{} - {}", e.display_artist(), e.full_title())) else {
        app.warn("The track has no name to ask for");
        return;
    };
    send_ask(app, entry, text);
}

/// What to ask for on behalf of a playlist track: "Artist - Title" from the tags it had when it was
/// last read, the title alone when that is all there is, else the file name without its extension.
fn playlist_query(e: &PlaylistEntry) -> Option<String> {
    let tag = |t: &Option<String>| t.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    let text = match (tag(&e.info.artist), tag(&e.info.title)) {
        (Some(artist), Some(title)) => format!("{artist} - {title}"),
        (None, Some(title)) => title,
        _ => e.path.file_stem()?.to_string_lossy().trim().to_owned(),
    };
    (!text.is_empty()).then_some(text)
}

/// Ask the bot for a playlist track, which is usually one whose file is not found any more.
pub fn ask_playlist_entry(app: &mut App, entry: EntryId) {
    let Some(text) = app.player.viewed().get(entry).and_then(playlist_query) else {
        app.warn("The track has no name to ask for");
        return;
    };
    send_ask(app, entry, text);
}

/// Ask the bot for `text` on behalf of `entry`.
fn send_ask(app: &mut App, entry: EntryId, text: String) {
    let Some(credentials) = credentials(&app.settings) else { return };
    let request = app.workers.request_id();
    let (bot, dir) = (bot(&app.settings).to_owned(), download_dir(app));
    app.info(format!("Asking @{bot} for {text}…"));
    app.workers.telegram.ask(request, credentials, Ask { bot, text: text.clone(), wait: WAIT, dir });
    app.telegram.asking = Some(Asking { request, entry, query: text });
}

/// Ask again with the query the user edited, dropping the results that were not good enough.
fn ask_again(app: &mut App, entry: EntryId, text: String) {
    app.workers.telegram.cancel_ask();
    app.telegram.choosing = None;
    send_ask(app, entry, text);
}

pub fn cancel_ask(app: &mut App) {
    app.workers.telegram.cancel_ask();
    app.telegram.asking = None;
    app.telegram.choosing = None;
    app.info("Stopped waiting for Telegram");
}

pub fn on_event(app: &mut App, event: Event) {
    match event {
        Event::Account(Ok(Some(name))) => {
            app.telegram.account = Account::SignedIn(name);
            app.telegram.session_saved = true;
        }
        Event::Account(Ok(None)) => {
            app.telegram.account = Account::SignedOut;
            app.telegram.session_saved = false;
        }
        Event::Account(Err(e)) => {
            // Offline or Telegram unreachable: the login may well be good, so ask again next time.
            app.telegram.account = Account::Unknown;
            app.telegram.checked = false;
            app.telegram.error = Some(e);
        }
        Event::Login(result) => on_login(app, result),
        Event::SignedOut => {
            app.telegram.account = Account::SignedOut;
            app.telegram.session_saved = false;
            app.telegram.step = Step::Idle;
            app.info("Signed out of Telegram");
        }
        Event::Ask { request, progress } => on_progress(app, request, progress),
    }
}

fn on_login(app: &mut App, result: Result<LoginStep, String>) {
    let t = &mut app.telegram;
    match result {
        Ok(LoginStep::CodeSent) => (t.step, t.error) = (Step::Code, None),
        Ok(LoginStep::PasswordNeeded { hint }) => {
            t.hint = hint.clone();
            (t.step, t.error) = (Step::Password(hint), None);
        }
        Ok(LoginStep::SignedIn { name }) => {
            (t.step, t.error, t.session_saved) = (Step::Idle, None, true);
            t.code.clear();
            t.password.clear();
            app.info(format!("Signed in to Telegram as {name}"));
            app.telegram.account = Account::SignedIn(name);
        }
        // The connection is kept for a wrong code or password, so the user can try again; after any
        // other failure the next try finds no login waiting and says so.
        Err(e) => {
            if let Step::Working(what) = t.step {
                t.step = match what {
                    "checking the code" => Step::Code,
                    "checking the password" => Step::Password(t.hint.clone()),
                    _ => Step::Idle,
                };
            }
            t.error = Some(e);
        }
    }
}

/// The picture of one of the results being chosen from.
pub fn on_thumb(app: &mut App, request: u64, index: usize, thumb: crate::workers::Thumb) {
    let ctx = app.ctx.clone();
    let Some(choosing) = app.telegram.choosing.as_mut().filter(|c| c.request == request) else { return };
    let image = egui::ColorImage::from_rgba_unmultiplied(thumb.size, &thumb.rgba);
    choosing.pictures.insert(index, ctx.load_texture(format!("telegram-{request}-{index}"), image, egui::TextureOptions::LINEAR));
}

fn on_progress(app: &mut App, request: u64, progress: Progress) {
    if app.telegram.asking.as_ref().is_none_or(|a| a.request != request) {
        return;
    }
    let bot = bot(&app.settings).to_owned();
    match progress {
        Progress::Sent => app.info(format!("Asked @{bot}, waiting up to {} s for its results…", WAIT.as_secs())),
        Progress::Choose(choices) => {
            app.info(format!("@{bot} offers {} results: choose one", choices.len()));
            if let Some(Asking { entry, query, .. }) = &app.telegram.asking {
                app.telegram.choosing = Some(Choosing { request, entry: *entry, query: query.clone(), edited: query.clone(), choices, pictures: HashMap::new() });
            }
        }
        // The pictures come as `TelegramThumb`, decoded; see `on_thumb`.
        Progress::Thumb { .. } => {}
        // Other replies are in the log (Settings > Logs), which the crate writes.
        Progress::Reply(_) => {}
        Progress::Downloading(name) => app.info(format!("Downloading {name} from @{bot}…")),
        Progress::Done(result) => {
            let asked = app.telegram.asking.take();
            app.telegram.choosing = None;
            match result {
                Ok(Outcome::File(path)) => {
                    app.info(format!("Saved {}", path.display()));
                    add_to_list(app, path);
                }
                // The picker opens with no results, to edit the query and try again.
                Ok(Outcome::NoResults) => {
                    app.warn(format!("@{bot} has nothing for that track"));
                    if let Some(Asking { entry, query, .. }) = asked {
                        app.telegram.choosing = Some(Choosing { request, entry, query: query.clone(), edited: query, choices: Vec::new(), pictures: HashMap::new() });
                    }
                }
                Ok(Outcome::NoFile { replies: 0 }) => app.warn(format!("@{bot} did not answer within {} s", WAIT.as_secs())),
                Ok(Outcome::NoFile { replies }) => app.warn(format!("@{bot} sent no sound file within {} s ({replies} other replies, see Settings > Logs)", WAIT.as_secs())),
                Err(e) => app.error(format!("Telegram: {e}")),
            }
        }
    }
}

/// Put the file in the "Telegram" playlist, made when it is missing, without leaving the list the user is looking at.
fn add_to_list(app: &mut App, path: PathBuf) {
    let viewing = app.player.viewing;
    let list = match app.player.lists.iter().position(|d| d.playlist.name == LIST) {
        Some(i) => i,
        None => {
            playlist_view::new_playlist(app, LIST);
            let made = app.player.viewing;
            app.player.viewing = viewing;
            made
        }
    };
    app.add_paths(list, vec![path], None);
}

/// The list of results the bot offered, for the user to choose one to request, with the query to
/// edit and ask again when none of them is right.
pub fn chooser_modal(app: &mut App, ctx: &egui::Context) {
    let Some(mut choosing) = app.telegram.choosing.take() else { return };
    let (mut pick, mut search, mut close) = (None, false, false);
    let height = (ctx.content_rect().height() - 290.0).clamp(80.0, 380.0);
    let bot = bot(&app.settings).to_owned();
    egui::Modal::new(egui::Id::unique("telegram-choose")).show(ctx, |ui| {
        ui.set_width(widgets::modal_width(ctx, 520.0));
        ui.heading(if choosing.choices.is_empty() { "No results" } else { "Choose a result" });
        let count = match choosing.choices.len() {
            0 => format!("@{bot} has nothing for \"{}\"", choosing.query),
            n => format!("@{bot} offers {n} results for \"{}\"", choosing.query),
        };
        ui.label(RichText::new(count).small().color(theme::text_dim()));
        // The query as it would be typed in a chat, to correct when the title is a little off.
        // The bot's name takes the room of the field on a phone, so it goes on a line of its own there.
        let narrow = ui.available_width() < 420.0;
        if narrow {
            ui.label(RichText::new(format!("@{bot}")).color(theme::text_dim()));
        }
        ui.horizontal(|ui| {
            if !narrow {
                ui.label(RichText::new(format!("@{bot}")).color(theme::text_dim()));
            }
            let field = ui.add(egui::TextEdit::singleline(&mut choosing.edited).desired_width(ui.available_width() - 80.0));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!choosing.edited.trim().is_empty(), egui::Button::new("Search")).on_hover_text("Ask the bot again with this text").clicked() || enter) && !choosing.edited.trim().is_empty() {
                search = true;
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            for (i, c) in choosing.choices.iter().enumerate() {
                ui.horizontal(|ui| {
                    picture(ui, choosing.pictures.get(&i));
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 80.0);
                        ui.add(egui::Label::new(RichText::new(&c.title).strong()).truncate());
                        if !c.description.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&c.description).small().color(theme::text_dim())).truncate());
                        }
                        if let Some(file) = &c.file {
                            ui.label(RichText::new(file).small().color(theme::ok()));
                        }
                    });
                    // The bot may not deliver, so this asks for the file rather than fetching it.
                    let hover = if c.file.is_some() { "Request this file from the bot" } else { "The result has no sound file of its own: it is sent to the chat with the bot, and the file is taken from there if one comes" };
                    if ui.button("Request").on_hover_text(hover).clicked() {
                        pick = Some(i);
                    }
                });
                ui.separator();
            }
        });
        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
    });
    if let Some(index) = pick {
        app.workers.telegram.choose(choosing.request, index);
        app.info("Requesting the file…");
    } else if search {
        ask_again(app, choosing.entry, choosing.edited.trim().to_owned());
    } else if close {
        cancel_ask(app);
    } else {
        app.telegram.choosing = Some(choosing);
    }
}

/// A result's picture in a square, fitted inside it; an empty square until the picture has arrived.
fn picture(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>) {
    const SIDE: f32 = 44.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SIDE, SIDE), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, theme::raised());
    if let Some(texture) = texture {
        let size = texture.size_vec2();
        let fitted = egui::Rect::from_center_size(rect.center(), size * (SIDE / size.x.max(size.y).max(1.0)));
        ui.painter().image(texture.id(), fitted, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
}

/// Ask Telegram whether the saved login still works, once per run, when the tab is shown.
fn check_once(app: &mut App) {
    let t = &app.telegram;
    if t.checked || !t.session_saved {
        return;
    }
    let Some(credentials) = credentials(&app.settings) else { return };
    app.telegram.checked = true;
    app.telegram.account = Account::Checking;
    app.workers.telegram.check(credentials);
}

/// The Settings > Telegram tab.
pub fn body(app: &mut App, ui: &mut egui::Ui) {
    check_once(app);
    let creds = credentials(&app.settings);
    let mut dirty = false;

    ui.horizontal(|ui| {
        ui.label("API id:");
        dirty |= ui.add(egui::TextEdit::singleline(&mut app.settings.telegram_api_id).hint_text("12345678").desired_width(80.0)).changed();
        ui.label("API hash:");
        dirty |= ui.add(egui::TextEdit::singleline(&mut app.settings.telegram_api_hash).password(true).hint_text("32 characters").desired_width(250.0)).changed();
        ui.hyperlink_to(RichText::new("my.telegram.org").small(), "https://my.telegram.org/apps").on_hover_text("Log in there and create an application to get both");
    });
    ui.horizontal(|ui| {
        ui.label("Phone:");
        dirty |= ui.add(egui::TextEdit::singleline(&mut app.settings.telegram_phone).hint_text("+49 170 1234567").desired_width(140.0)).changed();
        login_controls(app, ui, creds.as_ref());
    });
    if let Some(e) = &app.telegram.error {
        ui.label(RichText::new(e).small().color(theme::err()));
    }
    ui.horizontal(|ui| {
        ui.label("Bot:");
        dirty |= ui.add(egui::TextEdit::singleline(&mut app.settings.telegram_bot).hint_text(crate::store::DEFAULT_TELEGRAM_BOT).desired_width(190.0)).changed();
        ui.label("Save files to:");
        let dir = download_dir(app);
        ui.label(RichText::new(dir.display().to_string()).small().weak()).on_hover_text(dir.display().to_string());
        if ui.small_button("Choose…").clicked() {
            dialogs::choose_telegram_dir(app);
        }
        if app.settings.telegram_dir.is_some() && ui.small_button("Default").clicked() {
            app.settings.telegram_dir = None;
            dirty = true;
        }
    });
    ui.label(RichText::new(format!("Setlist > right-click a track, or a playlist track whose file is not found > Request on Telegram asks the bot for \"Artist - Title\" as you, like typing @bot Artist - Title in a chat, and saves the result you choose (or the only one) into a Telegram playlist, waiting up to {} s for the bot; everything the bot answers is logged on the Logs tab. Your login stays on this computer. Telegram may limit accounts that send automated messages, so this sends one per click.", WAIT.as_secs())).small().weak());
    if dirty {
        app.settings.telegram_api_id = app.settings.telegram_api_id.trim().to_owned();
        app.settings.telegram_api_hash = app.settings.telegram_api_hash.trim().to_owned();
        app.settings.telegram_bot = bot(&app.settings).to_owned();
        app.mark_settings_dirty();
        // New credentials are a new question for Telegram.
        app.telegram.checked = false;
    }
}

/// The part of the sign-in row after the phone number, for the step the sign-in is at.
fn login_controls(app: &mut App, ui: &mut egui::Ui, creds: Option<&Credentials>) {
    match &app.telegram.step {
        Step::Working(what) => {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(RichText::new(format!("Waiting for Telegram ({what})…")).small().weak());
        }
        Step::Code => {
            ui.label("Code:");
            let field = ui.add(egui::TextEdit::singleline(&mut app.telegram.code).hint_text("login code").desired_width(100.0));
            let go = ui.button("Sign in").clicked() || field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if go && !app.telegram.code.trim().is_empty() {
                app.telegram.step = Step::Working("checking the code");
                app.workers.telegram.sign_in(app.telegram.code.clone());
            }
            if ui.small_button("Cancel").clicked() {
                cancel_login(app);
            }
        }
        Step::Password(hint) => {
            ui.label("Password:");
            let hint = hint.as_ref().map(|h| format!("hint: {h}")).unwrap_or_else(|| "two-step password".into());
            let field = ui.add(egui::TextEdit::singleline(&mut app.telegram.password).password(true).hint_text(hint).desired_width(150.0));
            let go = ui.button("Sign in").clicked() || field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if go && !app.telegram.password.is_empty() {
                app.telegram.step = Step::Working("checking the password");
                app.workers.telegram.password(app.telegram.password.clone());
            }
            if ui.small_button("Cancel").clicked() {
                cancel_login(app);
            }
        }
        Step::Idle => match &app.telegram.account {
            Account::SignedIn(name) => {
                ui.label(RichText::new(format!("Signed in as {name}")).small().color(theme::ok()));
                if let Some(creds) = creds {
                    if ui.small_button("Sign out").on_hover_text("Signs this computer out of Telegram and deletes the saved login").clicked() {
                        app.telegram.step = Step::Working("signing out");
                        app.workers.telegram.sign_out(creds.clone());
                    }
                }
            }
            account => {
                if *account == Account::Checking {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new("Checking the saved login…").small().weak());
                }
                let phone = app.settings.telegram_phone.trim().to_owned();
                let why_not = match creds {
                    None => Some("Enter the API id and hash first"),
                    Some(_) if phone.is_empty() => Some("Enter the phone number first"),
                    Some(_) => None,
                };
                let button = ui.add_enabled(why_not.is_none(), egui::Button::new("Send code"));
                let hover = why_not.unwrap_or("Telegram sends a code to your other Telegram apps");
                if button.on_hover_text(hover).on_disabled_hover_text(hover).clicked() {
                    if let Some(creds) = creds {
                        app.telegram.error = None;
                        app.telegram.code.clear();
                        app.telegram.password.clear();
                        app.telegram.step = Step::Working("asking for a code");
                        app.workers.telegram.request_code(creds.clone(), phone);
                    }
                }
            }
        },
    }
}

fn cancel_login(app: &mut App) {
    app.workers.telegram.cancel_login();
    app.telegram.step = Step::Idle;
    app.telegram.error = None;
    app.telegram.code.clear();
    app.telegram.password.clear();
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;

    use super::*;
    use crate::ui::App;
    use crate::ui_snapshots::{seeded_store, settle};

    #[test]
    fn credentials_need_a_numeric_id_and_a_hash() {
        let mut s = Settings::default();
        assert!(credentials(&s).is_none());
        s.telegram_api_id = " 12345 ".into();
        assert!(credentials(&s).is_none());
        s.telegram_api_hash = "abc".into();
        let c = credentials(&s).unwrap();
        assert_eq!((c.api_id, c.api_hash.as_str()), (12345, "abc"));
        s.telegram_api_id = "twelve".into();
        assert!(credentials(&s).is_none());
    }

    /// A playlist track is asked for by its tags, or by its file name when it has none.
    #[test]
    fn a_playlist_track_is_asked_for_by_tags_or_file_name() {
        let mut e = PlaylistEntry::new("/gone/01 Locklead - Hunting.mp3".into());
        assert_eq!(playlist_query(&e).as_deref(), Some("01 Locklead - Hunting"));
        e.info.title = Some(" Hunting ".into());
        assert_eq!(playlist_query(&e).as_deref(), Some("Hunting"));
        e.info.artist = Some("Locklead".into());
        assert_eq!(playlist_query(&e).as_deref(), Some("Locklead - Hunting"));
        e.info.title = Some("  ".into());
        assert_eq!(playlist_query(&e).as_deref(), Some("01 Locklead - Hunting"));
    }

    #[test]
    fn the_bot_may_be_typed_with_an_at_sign() {
        let mut s = Settings::default();
        s.telegram_bot = " @some_bot ".into();
        assert_eq!(bot(&s), "some_bot");
    }

    /// A file the bot sent goes into a "Telegram" playlist, made once, without taking the user away
    /// from the list they are looking at; other outcomes only say what happened, and an answer to a
    /// question that is no longer running is dropped.
    #[test]
    fn the_bots_answer_lands_in_the_telegram_playlist() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        settle(&mut h, 300);
        let app = h.state_mut();
        app.settings.telegram_bot = "some_bot".into();
        let lists = app.player.lists.len();
        let file = app.player.lists[0].playlist.entries()[0].path.clone();
        let entry = app.player.lists[0].playlist.entries()[0].id;
        assert!(app.player.lists.iter().all(|d| d.playlist.name != LIST));

        let done = |app: &mut App, request: u64, result: Result<Outcome, String>| on_event(app, Event::Ask { request, progress: Progress::Done(result) });
        app.telegram.asking = Some(Asking { query: String::new(), request: 5, entry });
        done(app, 5, Ok(Outcome::File(file.clone())));
        assert!(app.telegram.asking.is_none());
        assert_eq!(app.player.lists.len(), lists + 1);
        let made = app.player.lists.iter().position(|d| d.playlist.name == LIST).expect("the Telegram playlist");
        assert_eq!(app.player.lists[made].playlist.entries().iter().map(|e| e.path.clone()).collect::<Vec<_>>(), [file.clone()]);
        assert_eq!(app.player.viewing, 0, "the list the user was looking at stays in view");

        app.telegram.asking = Some(Asking { query: String::new(), request: 6, entry });
        done(app, 6, Ok(Outcome::File(file.clone())));
        assert_eq!(app.player.lists.len(), lists + 1, "the same playlist is used again");
        assert_eq!(app.player.lists[made].playlist.entries().len(), 2);

        app.telegram.asking = Some(Asking { query: String::new(), request: 7, entry });
        done(app, 7, Ok(Outcome::NoFile { replies: 0 }));
        assert!(app.message.as_ref().is_some_and(|m| m.text.contains("did not answer within 30 s")), "{:?}", app.message.as_ref().map(|m| &m.text));
        app.telegram.asking = Some(Asking { query: String::new(), request: 8, entry });
        done(app, 8, Ok(Outcome::NoFile { replies: 2 }));
        assert!(app.message.as_ref().is_some_and(|m| m.text.contains("2 other replies")));
        app.telegram.asking = Some(Asking { query: String::new(), request: 9, entry });
        done(app, 9, Err("Not signed in".into()));
        assert!(app.message.as_ref().is_some_and(|m| m.text.contains("Not signed in")));

        // Stale: a question was asked after the one that is answered.
        app.telegram.asking = Some(Asking { query: String::new(), request: 11, entry });
        done(app, 10, Ok(Outcome::File(file)));
        assert!(app.telegram.asking.is_some());
        assert_eq!(app.player.lists[made].playlist.entries().len(), 2);
    }

    /// The results are listed for the user while the ask waits; an answer to another request is
    /// dropped, and the end of the ask closes the list.
    #[test]
    fn the_results_wait_for_the_users_choice() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        let app = h.state_mut();
        let entry = app.player.lists[0].playlist.entries()[0].id;
        let choice = |t: &str| Choice { title: t.into(), description: String::new(), kind: "audio".into(), file: Some("MP3, 4.2 MB, 3:45".into()) };
        app.telegram.asking = Some(Asking { request: 3, entry, query: "A - B".into() });
        on_event(app, Event::Ask { request: 2, progress: Progress::Choose(vec![choice("stale")]) });
        assert!(app.telegram.choosing.is_none());
        on_event(app, Event::Ask { request: 3, progress: Progress::Choose(vec![choice("one"), choice("two")]) });
        let choosing = app.telegram.choosing.as_ref().expect("the list is shown");
        assert_eq!((choosing.query.as_str(), choosing.choices.len()), ("A - B", 2));
        assert!(app.telegram.asking.is_some(), "the ask is still going");
        // Pictures arrive one by one, for the list that is shown only.
        let picture = || crate::workers::Thumb { size: [1, 1], rgba: vec![255; 4] };
        on_thumb(app, 2, 0, picture());
        on_thumb(app, 3, 1, picture());
        assert_eq!(app.telegram.choosing.as_ref().map(|c| c.pictures.keys().copied().collect::<Vec<_>>()), Some(vec![1]));
        // No results: the list opens empty, with the query to edit and ask again.
        on_event(app, Event::Ask { request: 3, progress: Progress::Done(Ok(Outcome::NoResults)) });
        assert!(app.telegram.asking.is_none());
        let empty = app.telegram.choosing.as_ref().expect("the editor opens");
        assert_eq!((empty.choices.len(), empty.edited.as_str(), empty.entry), (0, "A - B", entry));
        assert!(app.message.as_ref().is_some_and(|m| m.text.contains("nothing for that track")));
        // Any other end closes it.
        app.telegram.asking = Some(Asking { request: 4, entry, query: "A - B".into() });
        on_event(app, Event::Ask { request: 4, progress: Progress::Done(Ok(Outcome::NoFile { replies: 0 })) });
        assert!(app.telegram.choosing.is_none() && app.telegram.asking.is_none());
    }

    /// The menu item waits for the setup and for the sign-in; a wrong code leaves the code step open.
    #[test]
    fn asking_needs_the_setup_and_the_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = seeded_store(dir.path(), "dark");
        let mut h = Harness::builder().with_size(egui::vec2(1180.0, 740.0)).build_eframe(|cc| App::new(cc, store));
        let app = h.state_mut();
        assert!(unavailable(app).is_some_and(|w| w.contains("Set up")));
        (app.settings.telegram_api_id, app.settings.telegram_api_hash) = ("1".into(), "abc".into());
        assert!(unavailable(app).is_some_and(|w| w.contains("Sign in")), "no saved login yet");
        on_event(app, Event::Login(Ok(LoginStep::SignedIn { name: "Jan".into() })));
        assert_eq!(app.telegram.account, Account::SignedIn("Jan".into()));
        assert_eq!(unavailable(app), None);
        on_event(app, Event::Account(Ok(None)));
        assert!(unavailable(app).is_some());

        app.telegram.step = Step::Working("checking the code");
        on_event(app, Event::Login(Err("Telegram did not accept that code".into())));
        assert_eq!(app.telegram.step, Step::Code);
        assert_eq!(app.telegram.error.as_deref(), Some("Telegram did not accept that code"));
        app.telegram.step = Step::Working("asking for a code");
        on_event(app, Event::Login(Err("PHONE_NUMBER_INVALID".into())));
        assert_eq!(app.telegram.step, Step::Idle);
    }
}
