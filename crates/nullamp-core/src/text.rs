//! Text decoding helpers for files of unknown encoding (playlists, cue sheets).

/// Decode bytes as UTF-8 (with or without BOM), falling back to Windows-1252.
///
/// Legacy `.m3u` and `.cue` files are frequently written in a Windows code page.
/// Windows-1252 is a superset of Latin-1 for printable characters and is the most
/// common encoding for such files, so it is the fallback.
pub fn decode_lossy(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Some(utf16) = decode_utf16_bom(bytes) {
        return utf16;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
    }
}

fn decode_utf16_bom(bytes: &[u8]) -> Option<String> {
    let le = match bytes {
        [0xFF, 0xFE, ..] => true,
        [0xFE, 0xFF, ..] => false,
        _ => return None,
    };
    let units = bytes[2..].chunks_exact(2).map(|c| {
        if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }
    });
    Some(char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect())
}

fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_with_bom() {
        assert_eq!(decode_lossy(b"\xEF\xBB\xBFBj\xC3\xB6rk"), "Björk");
    }

    #[test]
    fn falls_back_to_cp1252() {
        assert_eq!(decode_lossy(b"Bj\xF6rk \x96 Army"), "Björk – Army");
    }

    #[test]
    fn utf16_le() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "Hé".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_lossy(&bytes), "Hé");
    }
}
