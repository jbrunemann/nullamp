//! Building search queries from what is known locally, and scoring candidates.

/// Search text for a DJ mix from its tags and file name.
///
/// Dates, bracketed noise and separators are removed from file names, so
/// `Runeban - Rubbito (07-08-2019).mp3` searches for `Runeban Rubbito`.
pub fn mix_query(title: Option<&str>, artist: Option<&str>, file_stem: &str) -> String {
    let from_tags = match (artist.map(str::trim).filter(|s| !s.is_empty()), title.map(str::trim).filter(|s| !s.is_empty())) {
        (Some(a), Some(t)) if !t.to_lowercase().contains(&a.to_lowercase()) => Some(format!("{a} {t}")),
        (_, Some(t)) => Some(t.to_owned()),
        _ => None,
    };
    let raw = from_tags.unwrap_or_else(|| file_stem.to_owned());
    clean(&raw)
}

fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            _ if depth > 0 => {}
            '_' | '.' | '-' | '–' | '|' | '/' | ',' => out.push(' '),
            c => out.push(c),
        }
    }
    out.split_whitespace()
        .filter(|w| !looks_like_date_or_number(w))
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_date_or_number(w: &str) -> bool {
    // Pure numbers and timestamps like 22h29m03s carry no searchable meaning.
    let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
    digits > 0 && digits * 2 >= w.chars().count()
}

pub(crate) fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

/// Token overlap between a query and a candidate name, 0.0 ..= 1.0.
pub fn similarity(query: &str, candidate: &str) -> f64 {
    let q = tokens(query);
    let c = tokens(candidate);
    if q.is_empty() || c.is_empty() {
        return 0.0;
    }
    let hits = q.iter().filter(|t| c.contains(t)).count();
    // Weighted towards covering the query, lightly penalising long candidate names.
    let recall = hits as f64 / q.len() as f64;
    let precision = hits as f64 / c.len() as f64;
    0.8 * recall + 0.2 * precision
}

/// Rank by name similarity, boosted when durations agree within 2 %.
pub fn rank_score(query: &str, candidate_name: &str, file_duration: Option<f64>, candidate_duration: Option<f64>) -> f64 {
    let mut score = similarity(query, candidate_name);
    if let (Some(a), Some(b)) = (file_duration, candidate_duration)
        && a > 0.0
        && b > 0.0
    {
        let ratio = (a - b).abs() / a.max(b);
        if ratio < 0.02 {
            score += 0.3;
        } else if ratio > 0.25 {
            score -= 0.2;
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_queries() {
        assert_eq!(mix_query(None, None, "Runeban - Rubbito (07-08-2019)"), "Runeban Rubbito");
        assert_eq!(mix_query(None, None, "2019-07-29_22h29m03s"), "");
        assert_eq!(mix_query(Some("Live at Fabric"), Some("Craig Richards"), "x"), "Craig Richards Live at Fabric");
        assert_eq!(mix_query(Some("Craig Richards - Live"), Some("Craig Richards"), "x"), "Craig Richards Live");
        assert_eq!(mix_query(Some(" "), None, "dj_set.final"), "dj set final");
    }

    #[test]
    fn scoring() {
        assert!(similarity("solomun boiler room", "Solomun - Boiler Room @ Tulum") > similarity("solomun boiler room", "Boiler Room Berlin"));
        assert_eq!(similarity("", "x"), 0.0);
        let close = rank_score("a b", "a b", Some(3600.0), Some(3610.0));
        let far = rank_score("a b", "a b", Some(3600.0), Some(1800.0));
        assert!(close > far);
    }
}
