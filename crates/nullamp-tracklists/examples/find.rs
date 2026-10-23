//! Searches 1001Tracklists and prints the first result's tracks.
//! `TRACKLISTS_GUID=... cargo run -p nullamp-tracklists --example find -- "Miss Monique Atlas Weekend 2020"`

fn main() {
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let mut hits = nullamp_tracklists::search(&query).unwrap_or_else(|e| panic!("search: {e}"));
    let year = query.split_whitespace().find_map(|w| w.parse::<u16>().ok().filter(|y| (1900..=2100).contains(y)));
    nullamp_tracklists::rank(&mut hits, &nullamp_tracklists::Wanted { query: &query, year, seconds: None });
    for hit in hits.iter().take(5) {
        println!("{}{} | {} | {:?} tracks | {:?} | score {}\n  {}", if hit.likely { "* " } else { "" }, hit.title, hit.date.as_deref().unwrap_or("-"), hit.tracks, hit.duration, hit.score, hit.url);
    }
    let (Some(first), Ok(guid)) = (hits.first(), std::env::var("TRACKLISTS_GUID")) else { return };
    let list = nullamp_tracklists::fetch(&guid, &first.url).unwrap_or_else(|e| panic!("fetch: {e}"));
    println!("\n{} / {:?} / {:?}\n{}", list.title, list.dj, list.date, list.text());
}
