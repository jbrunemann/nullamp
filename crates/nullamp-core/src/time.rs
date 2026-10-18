//! Formatting and parsing of human-readable timestamps.

/// Format seconds as `m:ss` or `h:mm:ss`. Negative and non-finite values clamp to zero.
pub fn format_clock(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 { secs.floor() as u64 } else { 0 };
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Like [`format_clock`] but with tenths of a second, for timestamp editing.
pub fn format_precise(secs: f64) -> String {
    let secs = if secs.is_finite() && secs > 0.0 { secs } else { 0.0 };
    let tenths = (secs * 10.0).round() as u64;
    let whole = tenths / 10;
    let (h, m, s) = (whole / 3600, (whole / 60) % 60, whole % 60);
    let frac = tenths % 10;
    if h > 0 { format!("{h}:{m:02}:{s:02}.{frac}") } else { format!("{m}:{s:02}.{frac}") }
}

/// Parse `ss`, `m:ss`, `mm:ss`, `h:mm:ss` and the same with a fractional second part.
///
/// Minutes may exceed 59 when no hour field is present (`75:30` is common in
/// tracklists for long mixes). Surrounding brackets and whitespace are ignored.
pub fn parse_timestamp(input: &str) -> Option<f64> {
    let s = input.trim().trim_start_matches(['[', '(']).trim_end_matches([']', ')']).trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (last, rest) = parts.split_last()?;
    let seconds: f64 = parse_unsigned_decimal(last)?;
    let mut total = 0.0;
    for (i, part) in rest.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value: f64 = part.parse().ok()?;
        // In h:mm:ss the minutes field must be a valid minute value.
        if rest.len() == 2 && i == 1 && value >= 60.0 {
            return None;
        }
        total = total * 60.0 + value;
    }
    if !rest.is_empty() && seconds >= 60.0 {
        return None;
    }
    Some(total * 60.0 + seconds)
}

fn parse_unsigned_decimal(s: &str) -> Option<f64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    if s.bytes().filter(|&b| b == b'.').count() > 1 || s.starts_with('.') {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_clock(0.0), "0:00");
        assert_eq!(format_clock(65.9), "1:05");
        assert_eq!(format_clock(3723.0), "1:02:03");
        assert_eq!(format_clock(-4.0), "0:00");
        assert_eq!(format_clock(f64::NAN), "0:00");
        assert_eq!(format_precise(65.25), "1:05.3");
        assert_eq!(format_precise(3723.0), "1:02:03.0");
    }

    #[test]
    fn parses() {
        assert_eq!(parse_timestamp("45"), Some(45.0));
        assert_eq!(parse_timestamp("1:05"), Some(65.0));
        assert_eq!(parse_timestamp("75:30"), Some(4530.0));
        assert_eq!(parse_timestamp("1:02:03"), Some(3723.0));
        assert_eq!(parse_timestamp("[01:02:03.5]"), Some(3723.5));
        assert_eq!(parse_timestamp(" (0:10) "), Some(10.0));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "abc", "1:2:3:4", "1:60", "1:61:00", "-3", "1::2", "1.2.3", ".5"] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }

    #[test]
    fn round_trip() {
        for secs in [0.0, 59.0, 61.0, 3599.0, 3600.0, 7322.0] {
            assert_eq!(parse_timestamp(&format_clock(secs)), Some(secs));
        }
    }
}
