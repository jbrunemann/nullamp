//! Prints a public Spotify playlist: `cargo run -p nullamp-spotify --example playlist -- LINK`

fn main() {
    let link = std::env::args().nth(1).expect("a playlist link");
    let list = nullamp_spotify::fetch(&link).unwrap_or_else(|e| panic!("{e}"));
    println!("{} ({} tracks{})", list.name, list.tracks.len(), if list.cut { ", cut at the page limit" } else { "" });
    for t in &list.tracks {
        println!("{:>5} | {} - {}", t.duration.map_or("-".into(), |s| format!("{}:{:02}", s as u64 / 60, s as u64 % 60)), t.artist, t.title);
    }
}
