//! Asks a Telegram bot for a track as the user's own account, and downloads the sound file it
//! answers with. The question is an inline query, what typing "@bot Artist - Title" in a chat
//! sends, so the bot's answer is a list of results rather than a message.
//!
//! A bot's API cannot message another bot, and the user wants the question to come from them, so
//! this logs in as a user (MTProto, through `grammers`, with the API id and hash the user got from
//! my.telegram.org). Everything runs on one thread with its own tokio runtime; callers send
//! [`Command`]s through [`Telegram`] and get [`Event`]s from the `notify` closure, so the app never
//! waits on the network.
//!
//! A connection is made for each job and closed after it, except between asking for the login code
//! and finishing the sign-in, which have to share one. The session (see `session.rs`) is what makes
//! the next job sign in without a code.

mod session;
mod sound;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use grammers_client::media::{Document, Media, Photo, PhotoSize};
use grammers_client::tl;
use grammers_client::sender::{SenderPoolFatHandle, UpdatesConfiguration};
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_client::client::{LoginToken, PasswordToken, UpdateStream};
use grammers_session::storages::MemorySession;
use grammers_session::types::{PeerId, PeerRef};
use grammers_session::updates::UpdatesLike;
use tokio::sync::mpsc;

pub use sound::{file_name, is_sound};
use sound::describe_file;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Telegram(#[from] grammers_client::InvocationError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

fn other(text: impl Into<String>) -> Error {
    Error::Other(text.into())
}

/// What the user got from my.telegram.org under "API development tools".
#[derive(Clone, Debug)]
pub struct Credentials {
    pub api_id: i32,
    pub api_hash: String,
}

/// A track to ask a bot for.
#[derive(Clone, Debug)]
pub struct Ask {
    /// The bot's username, without the `@`.
    pub bot: String,
    /// The query, as typed after the bot's name in a chat ("@bot Artist - Title"): "Artist - Title".
    pub text: String,
    /// How long to wait for a sound file after sending.
    pub wait: Duration,
    /// The folder the file is saved in.
    pub dir: PathBuf,
}

enum Command {
    Check(Credentials),
    RequestCode(Credentials, String),
    SignIn(String),
    Password(String),
    CancelLogin,
    SignOut(Credentials),
    Ask { request: u64, credentials: Credentials, ask: Ask },
    Choose { request: u64, index: usize },
    CancelAsk,
}

/// What a job reports. Sent from the telegram thread.
#[derive(Debug)]
pub enum Event {
    /// The answer to [`Telegram::check`]: the name of the account that is signed in, `None` when
    /// nobody is.
    Account(Result<Option<String>, String>),
    /// The answer to [`Telegram::request_code`], [`Telegram::sign_in`] and [`Telegram::password`].
    Login(Result<LoginStep, String>),
    /// The answer to [`Telegram::sign_out`].
    SignedOut,
    /// Progress of an [`Telegram::ask`], under its request id. A cancelled request sends nothing more.
    Ask { request: u64, progress: Progress },
}

#[derive(Debug, PartialEq)]
pub enum LoginStep {
    /// Telegram sent a code to the user's other devices (or by SMS); [`Telegram::sign_in`] takes it.
    CodeSent,
    /// The account has two-step verification; [`Telegram::password`] takes the password.
    PasswordNeeded { hint: Option<String> },
    SignedIn { name: String },
}

#[derive(Debug)]
pub enum Progress {
    /// The question is asked; the wait for the answer starts.
    Sent,
    /// The picture of result number `index` of the list sent with [`Progress::Choose`], as the bytes
    /// of an image file (JPEG, PNG or WebP). They follow one by one, so the list can be shown first.
    Thumb { index: usize, bytes: Vec<u8> },
    /// The bot offers these results for the user to choose from; [`Telegram::choose`] takes the
    /// index of one, [`Telegram::cancel_ask`] none. Not sent when there is only one result, which is taken.
    Choose(Vec<Choice>),
    /// A reply that is not a sound file, for the log.
    Reply(String),
    /// A sound file is coming in under this name.
    Downloading(String),
    /// The job is over.
    Done(Result<Outcome, String>),
}

/// One of the results a bot offers for a query.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub title: String,
    pub description: String,
    /// What the bot says the result is: "audio", "article"...
    pub kind: String,
    /// What the file is when the result carries a sound file ("MP3, 4.2 MB, 3:45"), else `None`.
    pub file: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The sound file the bot answered with, saved here.
    File(PathBuf),
    /// The bot had no results for the query.
    NoResults,
    /// The wait ended without a sound file; `replies` other messages came in.
    NoFile { replies: usize },
}

/// The handle to the telegram thread.
pub struct Telegram {
    commands: mpsc::UnboundedSender<Command>,
    /// The ask that may keep running; any other request id stops at its next step.
    current: Arc<AtomicU64>,
}

impl Telegram {
    /// Start the thread. `session_file` holds the login between runs; `notify` is called from the
    /// thread for each [`Event`] and must be quick.
    pub fn start(session_file: PathBuf, notify: impl Fn(Event) + Send + Sync + 'static) -> Telegram {
        let (commands, rx) = mpsc::unbounded_channel();
        let current = Arc::new(AtomicU64::new(0));
        let ask_current = Arc::clone(&current);
        std::thread::Builder::new()
            .name("nullamp-telegram".into())
            .spawn(move || match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime.block_on(Actor { file: session_file, notify: Arc::new(notify), ask_current, login: None, pending: None }.run(rx)),
                Err(e) => log::error!("Telegram: cannot start the runtime: {e}"),
            })
            .expect("spawn telegram thread");
        Telegram { commands, current }
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Whether a session is saved. Cheap; the answer to "is anyone signed in" is [`Telegram::check`].
    pub fn has_session(session_file: &Path) -> bool {
        session::exists(session_file)
    }

    /// Ask Telegram whether the saved session is still signed in; answered with [`Event::Account`].
    pub fn check(&self, credentials: Credentials) {
        self.send(Command::Check(credentials));
    }

    /// Have Telegram send a login code for `phone` (with country code, e.g. `+49 ...`); answered
    /// with [`Event::Login`].
    pub fn request_code(&self, credentials: Credentials, phone: String) {
        self.send(Command::RequestCode(credentials, phone));
    }

    /// Finish the login with the code; answered with [`Event::Login`].
    pub fn sign_in(&self, code: String) {
        self.send(Command::SignIn(code));
    }

    /// Finish the login with the two-step verification password; answered with [`Event::Login`].
    pub fn password(&self, password: String) {
        self.send(Command::Password(password));
    }

    /// Drop a login that is waiting for its code or password.
    pub fn cancel_login(&self) {
        self.send(Command::CancelLogin);
    }

    /// Sign out of Telegram and delete the saved session; answered with [`Event::SignedOut`].
    pub fn sign_out(&self, credentials: Credentials) {
        self.send(Command::SignOut(credentials));
    }

    /// Send `ask.text` to the bot and wait `ask.wait` for a sound file. Answered with
    /// [`Event::Ask`] under `request`. A newer request replaces this one.
    pub fn ask(&self, request: u64, credentials: Credentials, ask: Ask) {
        self.current.store(request, Ordering::SeqCst);
        self.send(Command::Ask { request, credentials, ask });
    }

    /// Download (or send, see the crate's `ask` flow) result number `index` of the [`Progress::Choose`]
    /// list of ask `request`; answered with the rest of [`Event::Ask`].
    pub fn choose(&self, request: u64, index: usize) {
        self.send(Command::Choose { request, index });
    }

    /// Stop the running ask at its next step, or give up waiting for a choice.
    pub fn cancel_ask(&self) {
        self.current.store(0, Ordering::SeqCst);
        self.send(Command::CancelAsk);
    }
}

/// A connection to Telegram.
struct Conn {
    client: Client,
    handle: SenderPoolFatHandle,
    task: tokio::task::JoinHandle<()>,
    /// Updates the connection delivers; taken by the one job that listens.
    updates: Option<mpsc::Receiver<UpdatesLike>>,
    session: Arc<MemorySession>,
}

impl Conn {
    async fn open(credentials: &Credentials, file: &Path) -> Conn {
        let session = Arc::new(MemorySession::from(session::load(file)));
        let SenderPool { runner, updates, handle } = SenderPool::new(Arc::clone(&session), credentials.api_id);
        let client = Client::new(handle.clone());
        let task = tokio::spawn(async move {
            runner.run().await;
        });
        Conn { client, handle, task, updates: Some(updates), session }
    }

    /// The session as it is now, saved; a failure is logged, since the job itself went fine.
    fn save(&self, file: &Path) {
        if let Err(e) = session::save(file, &self.session) {
            log::warn!("Telegram: cannot save the session to {}: {e}", file.display());
        }
    }

    async fn close(self, file: &Path) {
        self.save(file);
        self.handle.quit();
        let _ = self.task.await;
    }

    async fn account_name(&self) -> Result<String, Error> {
        let me = self.client.get_me().await?;
        let name = me.full_name();
        Ok(if name.trim().is_empty() { me.username().map_or_else(|| "your account".to_owned(), |u| format!("@{u}")) } else { name })
    }
}

/// A login between asking for the code and finishing it.
struct Login {
    conn: Conn,
    step: LoginState,
}

enum LoginState {
    Code(LoginToken),
    Password(PasswordToken),
}

struct Actor {
    file: PathBuf,
    notify: Arc<dyn Fn(Event) + Send + Sync>,
    ask_current: Arc<AtomicU64>,
    login: Option<Login>,
    /// An ask whose results wait for the user's choice, with the connection they came on.
    pending: Option<Pending>,
}

/// An ask between its results and the user's choice.
struct Pending {
    request: u64,
    conn: Conn,
    search: Search,
    /// Fetches the pictures of the results; stopped when the choice is made.
    thumbs: tokio::task::JoinHandle<()>,
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        while let Some(command) = rx.recv().await {
            match command {
                Command::Check(credentials) => {
                    let result = self.check(&credentials).await.map_err(|e| e.to_string());
                    (self.notify)(Event::Account(result));
                }
                Command::RequestCode(credentials, phone) => {
                    let result = self.request_code(&credentials, &phone).await.map_err(|e| e.to_string());
                    (self.notify)(Event::Login(result));
                }
                Command::SignIn(code) => {
                    let result = self.sign_in(code.trim()).await;
                    (self.notify)(Event::Login(result));
                }
                Command::Password(password) => {
                    let result = self.password(&password).await;
                    (self.notify)(Event::Login(result));
                }
                Command::CancelLogin => self.drop_login().await,
                Command::SignOut(credentials) => {
                    self.sign_out(&credentials).await;
                    (self.notify)(Event::SignedOut);
                }
                Command::Ask { request, credentials, ask } => {
                    self.drop_pending().await;
                    if self.ask_current.load(Ordering::SeqCst) != request {
                        continue;
                    }
                    let result = self.ask(request, &credentials, &ask).await;
                    self.report(request, result);
                }
                Command::Choose { request, index } => {
                    let Some(Pending { request: asked, conn, search, thumbs }) = self.pending.take() else { continue };
                    thumbs.abort();
                    if asked != request || self.ask_current.load(Ordering::SeqCst) != request {
                        conn.close(&self.file).await;
                        continue;
                    }
                    let result = {
                        let alive = || self.ask_current.load(Ordering::SeqCst) == request;
                        let emit = |progress: Progress| (self.notify)(Event::Ask { request, progress });
                        take(&conn, search, index, &alive, &emit).await
                    };
                    conn.close(&self.file).await;
                    self.report(request, result);
                }
                Command::CancelAsk => self.drop_pending().await,
            }
        }
        self.drop_login().await;
        self.drop_pending().await;
    }

    /// Tell the caller how an ask ended. `Ok(None)` is a cancelled ask, or one waiting for a choice: nothing to say.
    fn report(&self, request: u64, result: Result<Option<Outcome>, Error>) {
        match result {
            Ok(None) => {}
            Ok(Some(outcome)) => (self.notify)(Event::Ask { request, progress: Progress::Done(Ok(outcome)) }),
            Err(e) => (self.notify)(Event::Ask { request, progress: Progress::Done(Err(e.to_string())) }),
        }
    }

    async fn drop_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.thumbs.abort();
            pending.conn.close(&self.file).await;
        }
    }

    async fn drop_login(&mut self) {
        if let Some(login) = self.login.take() {
            login.conn.close(&self.file).await;
        }
    }

    async fn check(&mut self, credentials: &Credentials) -> Result<Option<String>, Error> {
        let conn = Conn::open(credentials, &self.file).await;
        let name = async {
            if conn.client.is_authorized().await? { conn.account_name().await.map(Some) } else { Ok(None) }
        }
        .await;
        conn.close(&self.file).await;
        name
    }

    async fn request_code(&mut self, credentials: &Credentials, phone: &str) -> Result<LoginStep, Error> {
        self.drop_login().await;
        let conn = Conn::open(credentials, &self.file).await;
        let step = async {
            if conn.client.is_authorized().await? {
                return Ok(Err(conn.account_name().await?));
            }
            Ok(Ok(conn.client.request_login_code(phone, &credentials.api_hash).await?))
        }
        .await;
        match step {
            Ok(Ok(token)) => {
                self.login = Some(Login { conn, step: LoginState::Code(token) });
                Ok(LoginStep::CodeSent)
            }
            Ok(Err(name)) => {
                conn.close(&self.file).await;
                Ok(LoginStep::SignedIn { name })
            }
            Err(e) => {
                conn.close(&self.file).await;
                Err(e)
            }
        }
    }

    async fn sign_in(&mut self, code: &str) -> Result<LoginStep, String> {
        let Some(mut login) = self.login.take() else { return Err("No login is waiting for a code; ask for a new one".into()) };
        let LoginState::Code(token) = &login.step else {
            self.login = Some(login);
            return Err("The account needs its two-step verification password, not a code".into());
        };
        match login.conn.client.sign_in(token, code).await {
            Ok(user) => {
                let name = user.full_name();
                login.conn.close(&self.file).await;
                Ok(LoginStep::SignedIn { name })
            }
            Err(SignInError::PasswordRequired(password)) => {
                let hint = password.hint().map(str::to_owned);
                login.step = LoginState::Password(password);
                self.login = Some(login);
                Ok(LoginStep::PasswordNeeded { hint })
            }
            Err(SignInError::InvalidCode) => {
                self.login = Some(login);
                Err("Telegram did not accept that code".into())
            }
            Err(e) => {
                login.conn.close(&self.file).await;
                Err(match e {
                    SignInError::SignUpRequired => "This phone number has no Telegram account; create one in a Telegram app first".to_owned(),
                    e => e.to_string(),
                })
            }
        }
    }

    async fn password(&mut self, password: &str) -> Result<LoginStep, String> {
        let Some(mut login) = self.login.take() else { return Err("No login is waiting for a password; ask for a new code".into()) };
        if !matches!(login.step, LoginState::Password(_)) {
            self.login = Some(login);
            return Err("Telegram wants the login code first".into());
        }
        let LoginState::Password(token) = login.step else { unreachable!() };
        match login.conn.client.check_password(token, password.as_bytes()).await {
            Ok(user) => {
                let name = user.full_name();
                login.conn.close(&self.file).await;
                Ok(LoginStep::SignedIn { name })
            }
            Err(SignInError::InvalidPassword(token)) => {
                login.step = LoginState::Password(token);
                self.login = Some(login);
                Err("Telegram did not accept that password".into())
            }
            Err(e) => {
                login.conn.close(&self.file).await;
                Err(e.to_string())
            }
        }
    }

    async fn sign_out(&mut self, credentials: &Credentials) {
        self.drop_login().await;
        let conn = Conn::open(credentials, &self.file).await;
        if let Err(e) = conn.client.sign_out().await {
            log::warn!("Telegram: sign out: {e}");
        }
        conn.handle.quit();
        let _ = conn.task.await;
        if let Err(e) = std::fs::remove_file(&self.file) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Telegram: cannot delete the session file {}: {e}", self.file.display());
            }
        }
    }

    /// Run an ask on its own connection, up to the user's choice among the results. `None` when it
    /// was cancelled or is waiting for that choice.
    async fn ask(&mut self, request: u64, credentials: &Credentials, ask: &Ask) -> Result<Option<Outcome>, Error> {
        let mut conn = Conn::open(credentials, &self.file).await;
        let started = {
            let alive = || self.ask_current.load(Ordering::SeqCst) == request;
            let emit = |progress: Progress| (self.notify)(Event::Ask { request, progress });
            match start(&mut conn, ask, &alive, &emit).await {
                // One result is no choice.
                Ok(Started::Results(search)) if search.results.results.len() == 1 => take(&conn, search, 0, &alive, &emit).await.map(Started::Finished),
                other => other,
            }
        };
        match started {
            Ok(Started::Results(search)) => {
                let choices = search.results.results.iter().map(choice).collect();
                (self.notify)(Event::Ask { request, progress: Progress::Choose(choices) });
                let thumbs = tokio::spawn(fetch_thumbs(conn.client.clone(), search.results.results.clone(), request, Arc::clone(&self.notify), Arc::clone(&self.ask_current)));
                self.pending = Some(Pending { request, conn, search, thumbs });
                Ok(None)
            }
            Ok(Started::Finished(outcome)) => {
                conn.close(&self.file).await;
                Ok(outcome)
            }
            Err(e) => {
                conn.close(&self.file).await;
                Err(e)
            }
        }
    }
}

/// The most results of a bot that are offered to choose from.
pub const MAX_RESULTS: usize = 5;

/// How often a waiting ask looks at whether it was cancelled.
const TICK: Duration = Duration::from_millis(250);

/// The bot's results for a query, with what is needed to take one of them.
struct Search {
    bot_id: PeerId,
    bot_ref: PeerRef,
    updates: UpdateStream,
    results: tl::types::messages::BotResults,
    ask: Ask,
}

enum Started {
    /// The ask is over already (no results, or it was cancelled).
    Finished(Option<Outcome>),
    Results(Search),
}

/// Ask the bot for results, what typing "@bot query" in a chat does, and log them.
async fn start(conn: &mut Conn, ask: &Ask, alive: &dyn Fn() -> bool, emit: &dyn Fn(Progress)) -> Result<Started, Error> {
    let client = &conn.client;
    if !client.is_authorized().await? {
        return Err(other("Not signed in to Telegram; sign in on the Settings > Telegram tab"));
    }
    let bot = client.resolve_username(&ask.bot).await?.ok_or_else(|| other(format!("Telegram has no user @{}", ask.bot)))?;
    let bot_ref = bot.to_ref().await.map_err(|e| other(e.to_string()))?.ok_or_else(|| other(format!("Cannot message @{}", ask.bot)))?;
    let updates = conn.updates.take().ok_or_else(|| other("The connection has no update channel"))?;
    // Listen before asking, so nothing that follows can be missed.
    let updates = client.stream_updates(updates, UpdatesConfiguration { catch_up: false }).await.map_err(|e| other(e.to_string()))?;

    let deadline = tokio::time::Instant::now() + ask.wait;
    let mut tick = tokio::time::interval(TICK);
    log::info!("Telegram: asking @{} for \"{}\" (inline query), waiting {} s", ask.bot, ask.text, ask.wait.as_secs());
    emit(Progress::Sent);
    let request = tl::functions::messages::GetInlineBotResults { bot: bot_ref.into(), peer: bot_ref.into(), geo_point: None, query: ask.text.clone(), offset: String::new() };
    let query = client.invoke(&request);
    tokio::pin!(query);
    let tl::enums::messages::BotResults::Results(mut results) = loop {
        tokio::select! {
            answer = &mut query => break answer?,
            _ = tick.tick() => {
                if !alive() {
                    log::info!("Telegram: stopped waiting, cancelled");
                    return Ok(Started::Finished(None));
                }
                if tokio::time::Instant::now() >= deadline {
                    log::info!("Telegram: @{} did not answer the inline query in {} s", ask.bot, ask.wait.as_secs());
                    return Ok(Started::Finished(Some(Outcome::NoFile { replies: 0 })));
                }
            }
        }
    };
    log::info!(
        "Telegram: @{} offers {} results (query {}, gallery {}, more {:?}, switch to chat {})",
        ask.bot,
        results.results.len(),
        results.query_id,
        results.gallery,
        results.next_offset,
        results.switch_pm.is_some()
    );
    for (i, result) in results.results.iter().enumerate() {
        log::info!("Telegram: result {}: {}", i + 1, describe_result(result));
    }
    if results.results.is_empty() {
        return Ok(Started::Finished(Some(Outcome::NoResults)));
    }
    // The best come first; a longer list only makes the choice harder.
    if results.results.len() > MAX_RESULTS {
        log::info!("Telegram: keeping the first {MAX_RESULTS} results");
        results.results.truncate(MAX_RESULTS);
    }
    Ok(Started::Results(Search { bot_id: bot.id(), bot_ref, updates, results, ask: ask.clone() }))
}

/// Take result `index` of a search: a result that carries a sound file is downloaded as it is,
/// without sending anything; any other is sent to the chat with the bot, as choosing it in Telegram
/// does, and the file is taken from the message that comes of it.
async fn take(conn: &Conn, mut search: Search, index: usize, alive: &dyn Fn() -> bool, emit: &dyn Fn(Progress)) -> Result<Option<Outcome>, Error> {
    let client = &conn.client;
    let ask = &search.ask;
    let chosen = search.results.results.get(index).ok_or_else(|| other("That result is not on the list"))?;
    log::info!("Telegram: taking result {} of {}: {:?}", index + 1, search.results.results.len(), result_id(chosen));
    if let tl::enums::BotInlineResult::BotInlineMediaResult(media) = chosen {
        if let Some(document) = media.document.as_ref().and_then(inline_document) {
            if is_sound(document.mime_type(), document.name()) {
                let name = file_name(document.name(), document.performer(), document.audio_title(), document.mime_type());
                emit(Progress::Downloading(name.clone()));
                return Ok(download(client, &document, &ask.dir, &name, alive).await?.map(Outcome::File));
            }
        }
    }
    let id = result_id(chosen).to_owned();
    log::info!("Telegram: the result carries no sound file; sending it to the chat with @{}", ask.bot);
    let random_id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as i64);
    client
        .invoke(&tl::functions::messages::SendInlineBotResult {
            silent: false,
            background: false,
            clear_draft: false,
            hide_via: false,
            peer: search.bot_ref.into(),
            reply_to: None,
            random_id,
            query_id: search.results.query_id,
            id,
            schedule_date: None,
            send_as: None,
            quick_reply_shortcut: None,
            allow_paid_stars: None,
        })
        .await?;

    let deadline = tokio::time::Instant::now() + ask.wait;
    let mut tick = tokio::time::interval(TICK);
    // The text last logged for each message, so an edit that changes nothing is not emitted again.
    let mut seen: HashMap<i32, String> = HashMap::new();
    let mut updates_seen = 0usize;
    loop {
        if !alive() {
            log::info!("Telegram: stopped waiting, cancelled");
            return Ok(None);
        }
        tokio::select! {
            _ = tick.tick() => {
                if tokio::time::Instant::now() >= deadline {
                    log::info!("Telegram: the {} s are over, {updates_seen} updates came in, no sound file taken", ask.wait.as_secs());
                    return Ok(Some(Outcome::NoFile { replies: seen.len() }));
                }
            }
            update = search.updates.next() => {
                updates_seen += 1;
                let (kind, message) = match update? {
                    Update::NewMessage(message) => ("new message", message),
                    Update::MessageEdited(message) => ("edited message", message),
                    other => {
                        log::info!("Telegram: update: {}", shorten(&format!("{other:?}"), 600));
                        continue;
                    }
                };
                let media = message.media();
                let mut text = message.text().to_owned();
                log::info!(
                    "Telegram: {kind} {} in chat {:?} from {:?}{}: text {:?}, media {}, buttons {}",
                    message.id(),
                    message.peer_id(),
                    message.sender_id(),
                    if message.outgoing() { " (ours)" } else { "" },
                    shorten(&text, 400),
                    describe_media(media.as_ref()),
                    message.reply_markup().map_or_else(|| "none".to_owned(), |m| shorten(&format!("{m:?}"), 600)),
                );
                if message.peer_id() != search.bot_id {
                    log::info!("Telegram: ignored, it is not in the chat with @{}", ask.bot);
                    continue;
                }
                // What we sent ourselves counts only when it is the sound file: that is the chosen result.
                if let Some(Media::Document(document)) = &media {
                    let sound = is_sound(document.mime_type(), document.name());
                    log::info!("Telegram: the document {:?} ({}) {} a sound file", document.name(), document.mime_type().unwrap_or("no type"), if sound { "is" } else { "is not" });
                    if sound {
                        let name = file_name(document.name(), document.performer(), document.audio_title(), document.mime_type());
                        emit(Progress::Downloading(name.clone()));
                        return Ok(download(client, document, &ask.dir, &name, alive).await?.map(Outcome::File));
                    }
                }
                if message.outgoing() {
                    continue;
                }
                if text.is_empty() {
                    text = format!("[{}]", describe_media(media.as_ref()));
                }
                if message.reply_markup().is_some() {
                    text.push_str(" [with buttons]");
                }
                if seen.get(&message.id()) != Some(&text) {
                    seen.insert(message.id(), text.clone());
                    emit(Progress::Reply(text));
                }
            }
        }
    }
}

/// The most bytes of a picture that is fetched; they are covers, not wallpapers.
const THUMB_LIMIT: usize = 1 << 20;

/// Fetch the picture of each result in turn and hand it over, until the ask is over.
async fn fetch_thumbs(client: Client, results: Vec<tl::enums::BotInlineResult>, request: u64, notify: Arc<dyn Fn(Event) + Send + Sync>, current: Arc<AtomicU64>) {
    for (index, result) in results.iter().enumerate() {
        if current.load(Ordering::SeqCst) != request {
            return;
        }
        match thumbnail(&client, result).await {
            Ok(Some(bytes)) => {
                log::info!("Telegram: result {} has a picture of {} KB", index + 1, bytes.len().div_ceil(1024));
                notify(Event::Ask { request, progress: Progress::Thumb { index, bytes } });
            }
            Ok(None) => log::info!("Telegram: result {} has no picture ({})", index + 1, describe_picture(result)),
            Err(e) => log::info!("Telegram: result {}: cannot get its picture: {e}", index + 1),
        }
    }
}

/// The picture of a result: a thumbnail of its photo or document, or the web picture Telegram serves for it.
async fn thumbnail(client: &Client, result: &tl::enums::BotInlineResult) -> Result<Option<Vec<u8>>, Error> {
    match result {
        tl::enums::BotInlineResult::BotInlineMediaResult(m) => {
            let sizes = if let Some(photo) = &m.photo {
                Photo::from_raw(photo.clone()).thumbs()
            } else if let Some(document) = m.document.as_ref().and_then(inline_document) {
                document.thumbs()
            } else {
                return Ok(None);
            };
            let Some(size) = best_size(sizes) else { return Ok(None) };
            let mut chunks = client.iter_download(&size);
            let mut bytes = Vec::new();
            while let Some(chunk) = chunks.next().await? {
                bytes.extend(chunk);
                if bytes.len() > THUMB_LIMIT {
                    return Ok(None);
                }
            }
            Ok(Some(bytes))
        }
        tl::enums::BotInlineResult::Result(r) => match &r.thumb {
            Some(tl::enums::WebDocument::Document(d)) => web_file(client, d).await,
            // An address on some other server: not fetched, since that would tell the server who asks.
            _ => Ok(None),
        },
    }
}

/// The size to show in a row of the list: the one whose width is nearest 200 pixels; the tiny
/// blurred preview embedded in the message only when nothing else is there.
fn best_size(sizes: Vec<PhotoSize>) -> Option<PhotoSize> {
    let width = |size: &PhotoSize| match size {
        PhotoSize::Size(s) => Some(s.width),
        PhotoSize::Cached(s) => Some(s.width),
        PhotoSize::Progressive(s) => Some(s.width),
        _ => None,
    };
    let best = sizes.iter().filter_map(|s| width(s).map(|w| (w, s))).min_by_key(|(w, _)| (w - 200).abs()).map(|(_, s)| s.clone());
    best.or_else(|| sizes.into_iter().find(|s| matches!(s, PhotoSize::Stripped(_))))
}

/// A picture Telegram proxies for a bot's result, read through `upload.getWebFile`.
async fn web_file(client: &Client, document: &tl::types::WebDocument) -> Result<Option<Vec<u8>>, Error> {
    const PART: i32 = 128 * 1024;
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let request = tl::functions::upload::GetWebFile {
            location: tl::enums::InputWebFileLocation::Location(tl::types::InputWebFileLocation { url: document.url.clone(), access_hash: document.access_hash }),
            offset: bytes.len() as i32,
            limit: PART,
        };
        let answer = match client.invoke(&request).await {
            Err(grammers_client::InvocationError::Rpc(rpc)) if rpc.name == "FILE_MIGRATE" => client.invoke_in_dc(rpc.value.unwrap_or(0) as i32, &request).await?,
            other => other?,
        };
        let tl::enums::upload::WebFile::File(file) = answer;
        let got = file.bytes.len();
        bytes.extend(file.bytes);
        if got < PART as usize || bytes.len() > THUMB_LIMIT {
            break;
        }
    }
    Ok((bytes.len() <= THUMB_LIMIT && !bytes.is_empty()).then_some(bytes))
}

/// Where a result keeps its picture, for the log when there is none to get.
fn describe_picture(result: &tl::enums::BotInlineResult) -> String {
    match result {
        tl::enums::BotInlineResult::BotInlineMediaResult(m) => format!("photo {}, document {}", m.photo.is_some(), m.document.as_ref().and_then(inline_document).map_or_else(|| "none".to_owned(), |d| format!("with {} sizes", d.thumbs().len()))),
        tl::enums::BotInlineResult::Result(r) => format!("thumb {}", r.thumb.as_ref().map_or_else(|| "none".to_owned(), |t| shorten(&format!("{t:?}"), 200))),
    }
}

/// A result as the user sees it in the list to choose from.
fn choice(result: &tl::enums::BotInlineResult) -> Choice {
    let (id, kind, title, description, fallback, file) = match result {
        tl::enums::BotInlineResult::BotInlineMediaResult(m) => {
            let document = m.document.as_ref().and_then(inline_document).filter(|d| is_sound(d.mime_type(), d.name()));
            let file = document.as_ref().map(|d| describe_file(&file_name(d.name(), d.performer(), d.audio_title(), d.mime_type()), d.size(), d.duration()));
            let named = document.as_ref().and_then(|d| match (d.performer(), d.audio_title()) {
                (Some(p), Some(t)) => Some(format!("{p} - {t}")),
                (None, Some(t)) => Some(t.to_owned()),
                _ => d.name().map(str::to_owned),
            });
            (&m.id, &m.r#type, &m.title, &m.description, named, file)
        }
        tl::enums::BotInlineResult::Result(r) => (&r.id, &r.r#type, &r.title, &r.description, r.url.clone(), None),
    };
    let present = |t: &Option<String>| t.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    Choice { title: present(title).or(fallback).unwrap_or_else(|| id.clone()), description: present(description).unwrap_or_default(), kind: kind.clone(), file }
}

/// A document of an inline result as a [`Document`], which knows its name and type.
fn inline_document(document: &tl::enums::Document) -> Option<Document> {
    matches!(document, tl::enums::Document::Document(_)).then(|| {
        Document::from_raw_media(tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: false,
            document: Some(document.clone()),
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        })
    })
}

fn result_id(result: &tl::enums::BotInlineResult) -> &str {
    match result {
        tl::enums::BotInlineResult::Result(r) => &r.id,
        tl::enums::BotInlineResult::BotInlineMediaResult(m) => &m.id,
    }
}

/// An inline result, for the log.
fn describe_result(result: &tl::enums::BotInlineResult) -> String {
    match result {
        tl::enums::BotInlineResult::BotInlineMediaResult(m) => format!(
            "media {:?}, type {:?}, title {:?}, description {:?}, document {}, photo {}, on sending: {}",
            m.id,
            m.r#type,
            m.title,
            m.description,
            m.document.as_ref().map_or_else(|| "none".to_owned(), |d| inline_document(d).map_or_else(|| "empty".to_owned(), |d| describe_media(Some(&Media::Document(d))))),
            m.photo.is_some(),
            shorten(&format!("{:?}", m.send_message), 200),
        ),
        tl::enums::BotInlineResult::Result(r) => format!(
            "plain {:?}, type {:?}, title {:?}, description {:?}, url {:?}, content {}, on sending: {}",
            r.id,
            r.r#type,
            r.title,
            r.description,
            r.url,
            r.content.as_ref().map_or_else(|| "none".to_owned(), |c| shorten(&format!("{c:?}"), 300)),
            shorten(&format!("{:?}", r.send_message), 200),
        ),
    }
}

/// What a message carries, for the log.
fn describe_media(media: Option<&Media>) -> String {
    match media {
        None => "none".to_owned(),
        Some(Media::Document(d)) => format!(
            "document {:?}, type {}, {} bytes, {} s, performer {:?}, title {:?}",
            d.name(),
            d.mime_type().unwrap_or("unknown"),
            d.size().map_or_else(|| "?".to_owned(), |s| s.to_string()),
            d.duration().map_or_else(|| "?".to_owned(), |s| format!("{s:.0}")),
            d.performer(),
            d.audio_title(),
        ),
        Some(Media::Photo(_)) => "photo".to_owned(),
        Some(other) => shorten(&format!("{other:?}"), 300),
    }
}

/// `text` cut to `max` characters, for log lines.
fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Save a document in `dir` under `name` (or the next free name). Written to a `.part` file first,
/// so an interrupted download never looks like a track. `None` when cancelled.
async fn download(client: &Client, document: &Document, dir: &Path, name: &str, alive: &dyn Fn() -> bool) -> Result<Option<PathBuf>, Error> {
    std::fs::create_dir_all(dir)?;
    let path = sound::unique_path(dir, name);
    let part = path.with_file_name(format!(".{}.part", path.file_name().and_then(|n| n.to_str()).unwrap_or("download")));
    let mut file = std::fs::File::create(&part)?;
    let mut chunks = client.iter_download(document);
    let mut bytes = 0u64;
    let finished = async {
        while let Some(chunk) = chunks.next().await? {
            if !alive() {
                return Ok::<bool, Error>(false);
            }
            file.write_all(&chunk)?;
            bytes += chunk.len() as u64;
        }
        file.flush()?;
        Ok(true)
    }
    .await;
    drop(file);
    match finished {
        Ok(true) => {
            std::fs::rename(&part, &path)?;
            log::info!("Telegram: saved {} ({} KB)", path.display(), bytes / 1024);
            Ok(Some(path))
        }
        failed => {
            let _ = std::fs::remove_file(&part);
            failed.map(|_| None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(id: &str, title: Option<&str>, description: Option<&str>, url: Option<&str>) -> tl::enums::BotInlineResult {
        tl::enums::BotInlineResult::Result(tl::types::BotInlineResult {
            id: id.into(),
            r#type: "article".into(),
            title: title.map(Into::into),
            description: description.map(Into::into),
            url: url.map(Into::into),
            thumb: None,
            content: None,
            send_message: tl::enums::BotInlineMessage::MediaAuto(tl::types::BotInlineMessageMediaAuto { invert_media: false, message: String::new(), entities: None, reply_markup: None }),
        })
    }

    /// A result is listed by its title, else its address, else its id; blank text counts as none.
    #[test]
    fn results_are_listed_by_title_address_or_id() {
        let c = choice(&plain("a1", Some(" Hunting "), Some("Artist: Locklead"), None));
        assert_eq!((c.title.as_str(), c.description.as_str(), c.kind.as_str(), c.file), ("Hunting", "Artist: Locklead", "article", None));
        assert_eq!(choice(&plain("a2", Some("  "), None, Some("https://x.test/a"))).title, "https://x.test/a");
        assert_eq!(choice(&plain("a3", None, Some(""), None)).title, "a3");
    }
}
