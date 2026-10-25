//! Signs in (once; the session is kept in `ask.session` here) and asks a bot for a track, the way
//! the app does, to try the real thing without the UI.
//!
//! ```sh
//! TG_ID=123 TG_HASH=abc TG_PHONE=+49... cargo run -p nullamp-telegram --example ask -- "Artist - Title" [BOT] [DIR]
//! ```
//!
//! `ask.session` holds the login of your account: delete it when done.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use nullamp_telegram::{Ask, Credentials, Event, LoginStep, Outcome, Progress, Telegram};

fn prompt(label: &str) -> String {
    print!("{label}: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).expect("stdin");
    line.trim().to_owned()
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1);
    let text = args.next().expect("usage: ask \"Artist - Title\" [BOT] [DIR]");
    let bot = args.next().unwrap_or_else(|| "inreach_to_telegram_bot".into());
    let dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let credentials = Credentials { api_id: std::env::var("TG_ID").expect("TG_ID").parse().expect("TG_ID is a number"), api_hash: std::env::var("TG_HASH").expect("TG_HASH") };

    let (tx, rx) = mpsc::channel();
    let telegram = Telegram::start(PathBuf::from("ask.session"), move |event| {
        let _ = tx.send(event);
    });

    telegram.check(credentials.clone());
    let Event::Account(account) = rx.recv().unwrap() else { unreachable!() };
    match account {
        Ok(Some(name)) => println!("Signed in as {name}"),
        Ok(None) => {
            telegram.request_code(credentials.clone(), std::env::var("TG_PHONE").expect("TG_PHONE for the first sign-in"));
            loop {
                let Event::Login(step) = rx.recv().unwrap() else { continue };
                match step {
                    Ok(LoginStep::CodeSent) => telegram.sign_in(prompt("Login code")),
                    Ok(LoginStep::PasswordNeeded { hint }) => telegram.password(prompt(&format!("Password (hint: {})", hint.unwrap_or_default()))),
                    Ok(LoginStep::SignedIn { name }) => break println!("Signed in as {name}"),
                    Err(e) => panic!("{e}"),
                }
            }
        }
        Err(e) => panic!("{e}"),
    }

    telegram.ask(1, credentials, Ask { bot, text, wait: Duration::from_secs(30), dir });
    loop {
        let Event::Ask { progress, .. } = rx.recv().unwrap() else { continue };
        match progress {
            Progress::Sent => println!("Sent, waiting..."),
            Progress::Choose(choices) => {
                for (i, c) in choices.iter().enumerate() {
                    println!("{}. {} | {} | {} | {}", i + 1, c.title, c.description, c.kind, c.file.as_deref().unwrap_or("no file"));
                }
                let n: usize = prompt("Download which").parse().unwrap_or(1);
                telegram.choose(1, n.saturating_sub(1));
            }
            Progress::Thumb { .. } => {}
            Progress::Reply(text) => println!("Reply: {text}"),
            Progress::Downloading(name) => println!("Downloading {name}"),
            Progress::Done(Ok(Outcome::File(path))) => break println!("Saved {}", path.display()),
            Progress::Done(Ok(Outcome::NoResults)) => break println!("The bot had no results"),
            Progress::Done(Ok(Outcome::NoFile { replies })) => break println!("No sound file ({replies} other replies)"),
            Progress::Done(Err(e)) => break println!("Failed: {e}"),
        }
    }
}
