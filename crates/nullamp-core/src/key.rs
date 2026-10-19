//! Musical keys as DJ software writes them into tags, and the two ways to show one.
//!
//! The ID3 `TKEY` frame (and its Vorbis and MP4 counterparts) holds text, and programs
//! disagree on the notation: rekordbox and Traktor write `Am`, Mixed In Key writes the
//! Camelot code `8A`, Traktor can also use Open Key (`1m`). All of them name the same 24 keys.

/// A major or minor key, whatever notation the tag used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// Pitch class of the tonic, C = 0.
    pitch: u8,
    minor: bool,
}

/// How a key is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyNotation {
    /// `8A`, the code on the Camelot wheel that harmonic mixing goes by.
    #[default]
    Camelot,
    /// `Am`.
    Musical,
}

const MAJOR: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const MINOR: [&str; 12] = ["Cm", "C#m", "Dm", "D#m", "Em", "Fm", "F#m", "Gm", "G#m", "Am", "Bbm", "Bm"];

impl Key {
    /// A key from a tag value in Camelot (`8A`), Open Key (`1m`) or musical notation (`Am`,
    /// `F# minor`, `Bb`). `None` for anything else, including the ID3 "off key" marker `o`.
    pub fn parse(text: &str) -> Option<Key> {
        let t = text.trim();
        let digits = t.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            return Self::parse_numbered(t[..digits].parse().ok()?, &t[digits..]);
        }
        let mut chars = t.chars();
        let base = match chars.next()?.to_ascii_uppercase() {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => return None,
        };
        let rest = chars.as_str();
        let (shift, rest) = if let Some(r) = rest.strip_prefix(['#', '♯']) {
            (1, r)
        } else if let Some(r) = rest.strip_prefix(['b', '♭']) {
            (11, r)
        } else {
            (0, rest)
        };
        let minor = match rest.trim().to_ascii_lowercase().as_str() {
            "" | "maj" | "major" => false,
            "m" | "min" | "minor" => true,
            _ => return None,
        };
        Some(Key { pitch: (base + shift) % 12, minor })
    }

    /// `n` and the letter that follows it in a Camelot (`A`/`B`) or Open Key (`m`/`d`) code.
    fn parse_numbered(n: u32, suffix: &str) -> Option<Key> {
        if !(1..=12).contains(&n) {
            return None;
        }
        let n = n as u8;
        let (camelot, minor) = match suffix {
            "A" | "a" => (n, true),
            "B" | "b" => (n, false),
            // Open Key counts the same circle of fifths from C major = 1d instead of 8B.
            "m" => ((n + 6) % 12 + 1, true),
            "d" => ((n + 6) % 12 + 1, false),
            _ => return None,
        };
        // Position on the circle of fifths: the major key at Camelot 8 is C, one step is a fifth.
        let fifths = (camelot + 4) % 12;
        let major = (fifths * 7) % 12;
        Some(Key { pitch: if minor { (major + 9) % 12 } else { major }, minor })
    }

    /// The Camelot number, 1 to 12.
    pub fn camelot_number(self) -> u8 {
        let major = if self.minor { (self.pitch + 3) % 12 } else { self.pitch };
        // 7 is its own inverse modulo 12 and turns a pitch class into steps along the fifths.
        (major * 7 % 12 + 7) % 12 + 1
    }

    pub fn camelot(self) -> String {
        format!("{}{}", self.camelot_number(), if self.minor { 'A' } else { 'B' })
    }

    pub fn musical(self) -> &'static str {
        if self.minor { MINOR[self.pitch as usize] } else { MAJOR[self.pitch as usize] }
    }

    pub fn display(self, notation: KeyNotation) -> String {
        match notation {
            KeyNotation::Camelot => self.camelot(),
            KeyNotation::Musical => self.musical().to_owned(),
        }
    }

    /// Order for sorting: along the Camelot wheel, the minor key before its relative major.
    pub fn wheel_order(self) -> u8 {
        self.camelot_number() * 2 + !self.minor as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_codes_match_the_wheel() {
        for (musical, camelot) in [
            ("C", "8B"),
            ("Am", "8A"),
            ("G", "9B"),
            ("Em", "9A"),
            ("F", "7B"),
            ("Dm", "7A"),
            ("B", "1B"),
            ("G#m", "1A"),
            ("F#", "2B"),
            ("D#m", "2A"),
            ("Db", "3B"),
            ("Bbm", "3A"),
            ("Ab", "4B"),
            ("Fm", "4A"),
            ("Eb", "5B"),
            ("Cm", "5A"),
            ("Bb", "6B"),
            ("Gm", "6A"),
            ("D", "10B"),
            ("E", "12B"),
            ("C#m", "12A"),
        ] {
            let key = Key::parse(musical).unwrap_or_else(|| panic!("{musical}"));
            assert_eq!(key.camelot(), camelot, "{musical}");
            assert_eq!(key.musical(), musical);
            assert_eq!(Key::parse(camelot), Some(key), "{camelot}");
        }
    }

    #[test]
    fn every_key_survives_both_notations() {
        for pitch in 0..12 {
            for minor in [false, true] {
                let key = Key { pitch, minor };
                assert_eq!(Key::parse(&key.camelot()), Some(key));
                assert_eq!(Key::parse(key.musical()), Some(key));
            }
        }
    }

    #[test]
    fn parses_what_programs_write() {
        let am = Key::parse("Am");
        assert!(am.is_some());
        for text in [" am ", "A minor", "A min", "08A"] {
            assert_eq!(Key::parse(text), am, "{text}");
        }
        assert_eq!(Key::parse("1m"), am, "Open Key 1m is A minor");
        assert_eq!(Key::parse("1d"), Key::parse("C"), "Open Key 1d is C major");
        assert_eq!(Key::parse("11d"), Key::parse("Bb"));
        assert_eq!(Key::parse("12m"), Key::parse("Dm"), "Open Key 12m is D minor");
        assert_eq!(Key::parse("C major"), Key::parse("C"));
        assert_eq!(Key::parse("C♯m"), Key::parse("Dbm"));
        assert_eq!(Key::parse("bb"), Key::parse("Bb"));
        assert_eq!(Key::parse("Bm").map(Key::camelot).as_deref(), Some("10A"));
        assert_eq!(Key::parse("E♭").map(Key::camelot).as_deref(), Some("5B"));
    }

    #[test]
    fn rejects_what_is_not_a_key() {
        for text in ["", "o", "H", "Am7", "0A", "13A", "8C", "8", "Cx", "128"] {
            assert_eq!(Key::parse(text), None, "{text}");
        }
    }

    #[test]
    fn wheel_order_runs_along_the_wheel() {
        let order = |s: &str| Key::parse(s).unwrap().wheel_order();
        assert!(order("G#m") < order("B") && order("B") < order("D#m"));
        assert!(order("Am") < order("C"), "minor first within a number");
        let mut all: Vec<u8> = (0..12).flat_map(|p| [false, true].map(|m| Key { pitch: p, minor: m }.wheel_order())).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 24);
    }
}
