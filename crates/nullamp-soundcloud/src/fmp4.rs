//! Minimal MP4 box walker, just enough to patch the duration fields of a CMAF init segment.
//!
//! When SoundCloud's HLS variants are concatenated as `init.mp4 + data000.m4s + data001.m4s + …`
//! the resulting file is a valid fragmented MP4 but its `moov/mvhd.duration` and
//! `moov/trak/mdia/mdhd.duration` fields stay at `0`: the init segment was written when no media
//! existed yet. symphonia (and others) report the first fragment's length instead of the whole set,
//! so the player's waveform spans ten seconds. We know the total up front from the EXTINF lines, so
//! we patch those two fields in the init bytes before writing them to disk.
//!
//! Patching is not enough on its own: every media segment also starts with a `styp` and a `sidx`
//! box (the index of that one segment). symphonia reads top-level boxes only up to the first
//! `moof`, so it sees just the first `sidx` and, for a fragmented file, prefers it over `mdhd`:
//! the length is again ten seconds. [`strip_segment_index`] drops those boxes from each segment, so
//! the patched `mdhd` is the only duration left.

use std::borrow::Cow;
use std::ops::Range;

/// The segment without its top-level `styp` and `sidx` boxes; a segment that has neither is
/// returned as it is, without a copy. A segment that does not parse is also left alone.
pub fn strip_segment_index(segment: &[u8]) -> Cow<'_, [u8]> {
    let mut kept: Vec<Range<usize>> = Vec::new();
    let mut dropped = false;
    let mut cursor = 0;
    while cursor < segment.len() {
        let Some(hdr) = read_box_header(segment, cursor) else { return Cow::Borrowed(segment) };
        let Some(end) = cursor.checked_add(hdr.size).filter(|&end| end <= segment.len()) else { return Cow::Borrowed(segment) };
        if matches!(&hdr.ty, b"styp" | b"sidx") {
            dropped = true;
        } else {
            kept.push(cursor..end);
        }
        cursor = end;
    }
    if !dropped {
        return Cow::Borrowed(segment);
    }
    Cow::Owned(kept.into_iter().flat_map(|r| segment[r].iter().copied()).collect())
}

/// Walk the init segment and overwrite the duration fields with `total_secs`-worth of ticks. Boxes
/// that are absent or shorter than their payload require are quietly skipped: a non-standard init
/// just doesn't get patched. Returns `true` when at least one field was written, for logging.
pub fn patch_init_duration(buf: &mut [u8], total_secs: f64) -> bool {
    let Some(moov) = find_top_level_box(buf, b"moov") else { return false };
    let mut patched = false;
    if let Some(mvhd) = find_child_box(buf, b"mvhd", moov.start + 8, moov.end) {
        patched |= patch_duration_in_header(buf, mvhd, total_secs);
    }
    let mut cursor = moov.start + 8;
    while cursor + 8 <= moov.end {
        let Some(hdr) = read_box_header(buf, cursor) else { break };
        let box_end = cursor.saturating_add(hdr.size);
        if box_end <= cursor || box_end > moov.end {
            break;
        }
        if &hdr.ty == b"trak"
            && let Some(mdia) = find_child_box(buf, b"mdia", cursor + 8, box_end)
            && let Some(mdhd) = find_child_box(buf, b"mdhd", mdia.start + 8, mdia.end)
        {
            patched |= patch_duration_in_header(buf, mdhd, total_secs);
        }
        cursor = box_end;
    }
    patched
}

#[derive(Debug)]
struct BoxHeader {
    ty: [u8; 4],
    /// Total size of the box, header included.
    size: usize,
}

fn read_box_header(buf: &[u8], at: usize) -> Option<BoxHeader> {
    if at + 8 > buf.len() {
        return None;
    }
    let size = u32::from_be_bytes(buf[at..at + 4].try_into().ok()?);
    let mut ty = [0u8; 4];
    ty.copy_from_slice(&buf[at + 4..at + 8]);
    let actual = match size {
        // 0 means "to end of file".
        0 => buf.len().saturating_sub(at),
        // 1 means a 64-bit largesize follows the 32-bit header.
        1 => {
            if at + 16 > buf.len() {
                return None;
            }
            u64::from_be_bytes(buf[at + 8..at + 16].try_into().ok()?) as usize
        }
        n => n as usize,
    };
    if actual < 8 {
        return None;
    }
    Some(BoxHeader { ty, size: actual })
}

fn find_top_level_box(buf: &[u8], target: &[u8; 4]) -> Option<Range<usize>> {
    find_child_box(buf, target, 0, buf.len())
}

fn find_child_box(buf: &[u8], target: &[u8; 4], start: usize, end: usize) -> Option<Range<usize>> {
    let mut cursor = start;
    while cursor + 8 <= end {
        let hdr = read_box_header(buf, cursor)?;
        let box_end = cursor.checked_add(hdr.size)?;
        if box_end > end {
            return None;
        }
        if &hdr.ty == target {
            return Some(cursor..box_end);
        }
        if hdr.size == 0 {
            return None;
        }
        cursor = box_end;
    }
    None
}

/// Patch the duration field of an `mvhd` or `mdhd` box (same layout for both). Returns `true` if a
/// non-zero duration was written.
fn patch_duration_in_header(buf: &mut [u8], range: Range<usize>, total_secs: f64) -> bool {
    let payload = range.start + 8;
    if payload >= range.end {
        return false;
    }
    let version = buf[payload];
    match version {
        0 => {
            // version(1) flags(3) creation(4) modification(4) timescale(4) duration(4)
            let ts = payload + 4 + 4 + 4;
            if ts + 8 > range.end {
                return false;
            }
            let timescale = u32::from_be_bytes(buf[ts..ts + 4].try_into().unwrap());
            if timescale == 0 {
                return false;
            }
            let duration = ((total_secs.max(0.0) * f64::from(timescale)).round() as u64).min(u32::MAX as u64) as u32;
            buf[ts + 4..ts + 8].copy_from_slice(&duration.to_be_bytes());
            duration > 0
        }
        1 => {
            // version(1) flags(3) creation(8) modification(8) timescale(4) duration(8)
            let ts = payload + 4 + 8 + 8;
            if ts + 12 > range.end {
                return false;
            }
            let timescale = u32::from_be_bytes(buf[ts..ts + 4].try_into().unwrap());
            if timescale == 0 {
                return false;
            }
            let duration = (total_secs.max(0.0) * f64::from(timescale)).round() as u64;
            buf[ts + 4..ts + 12].copy_from_slice(&duration.to_be_bytes());
            duration > 0
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal valid init segment with one `moov` holding one `mvhd` and one `trak/mdia/mdhd`.
    /// Both headers are version 0 with timescales 1000 and 48000 and durations zero.
    fn minimal_init() -> Vec<u8> {
        let mvhd = mvhd_v0(1000, 0);
        let mdhd = mdhd_v0(48000, 0);
        let mdia = box_bytes(b"mdia", &mdhd);
        let trak = box_bytes(b"trak", &mdia);
        let moov_payload: Vec<u8> = mvhd.iter().chain(trak.iter()).copied().collect();
        let moov = box_bytes(b"moov", &moov_payload);
        let ftyp = box_bytes(b"ftyp", b"iso5\x00\x00\x00\x00iso5mp41");
        ftyp.into_iter().chain(moov).collect()
    }

    fn box_bytes(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(size as usize);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(payload);
        out
    }

    fn mvhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
        // version 0: creation(4) modification(4) timescale(4) duration(4) + the rest of the standard payload (80 bytes).
        let mut payload = Vec::with_capacity(100);
        payload.push(0); // version
        payload.extend_from_slice(&[0, 0, 0]); // flags
        payload.extend_from_slice(&0u32.to_be_bytes()); // creation
        payload.extend_from_slice(&0u32.to_be_bytes()); // modification
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&duration.to_be_bytes());
        payload.extend_from_slice(&[0u8; 80]); // the rest (rate, volume, matrix, next_track_id, …)
        box_bytes(b"mvhd", &payload)
    }

    fn mdhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
        let mut payload = Vec::with_capacity(24);
        payload.push(0);
        payload.extend_from_slice(&[0, 0, 0]);
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&duration.to_be_bytes());
        payload.extend_from_slice(&[0u8; 4]); // language + pre_defined
        box_bytes(b"mdhd", &payload)
    }

    #[test]
    fn patches_mvhd_and_mdhd_version_zero_fields() {
        let mut buf = minimal_init();
        assert!(patch_init_duration(&mut buf, 9188.3));
        // Find mvhd and mdhd again and read their duration fields.
        let moov = find_top_level_box(&buf, b"moov").unwrap();
        let mvhd = find_child_box(&buf, b"mvhd", moov.start + 8, moov.end).unwrap();
        // Duration lives at payload + 16 (version 0: v+flags, creation, modification, timescale, duration).
        let mvhd_dur = u32::from_be_bytes(buf[mvhd.start + 8 + 16..mvhd.start + 8 + 20].try_into().unwrap());
        assert_eq!(mvhd_dur, (9188.3_f64 * 1000.0).round() as u32);

        let trak = find_child_box(&buf, b"trak", moov.start + 8, moov.end).unwrap();
        let mdia = find_child_box(&buf, b"mdia", trak.start + 8, trak.end).unwrap();
        let mdhd = find_child_box(&buf, b"mdhd", mdia.start + 8, mdia.end).unwrap();
        let mdhd_dur = u32::from_be_bytes(buf[mdhd.start + 8 + 16..mdhd.start + 8 + 20].try_into().unwrap());
        assert_eq!(mdhd_dur, (9188.3_f64 * 48000.0).round() as u32);
    }

    #[test]
    fn strips_styp_and_sidx_and_keeps_the_rest() {
        let moof = box_bytes(b"moof", b"fragment");
        let mdat = box_bytes(b"mdat", b"audio audio");
        let segment: Vec<u8> = [box_bytes(b"styp", b"msdh\0\0\0\0"), box_bytes(b"sidx", &[0u8; 36]), moof.clone(), mdat.clone()].concat();
        let stripped = strip_segment_index(&segment);
        assert_eq!(stripped.as_ref(), [moof, mdat].concat());
    }

    #[test]
    fn a_segment_without_an_index_is_not_copied() {
        let segment: Vec<u8> = [box_bytes(b"moof", b"fragment"), box_bytes(b"mdat", b"audio")].concat();
        assert!(matches!(strip_segment_index(&segment), Cow::Borrowed(_)));
    }

    #[test]
    fn a_truncated_segment_is_left_alone() {
        let mut segment = box_bytes(b"sidx", &[0u8; 36]);
        segment.extend_from_slice(&box_bytes(b"moof", b"fragment")[..10]);
        assert_eq!(strip_segment_index(&segment).as_ref(), segment.as_slice());
    }

    #[test]
    fn a_missing_moov_leaves_the_buffer_alone() {
        let mut buf = box_bytes(b"ftyp", b"iso5\x00\x00\x00\x00iso5");
        let before = buf.clone();
        assert!(!patch_init_duration(&mut buf, 100.0));
        assert_eq!(buf, before);
    }

    #[test]
    fn a_zero_timescale_does_not_divide_by_anything() {
        // Garbage mvhd with timescale 0; no patch should happen but no panic either.
        let mvhd = mvhd_v0(0, 0);
        let moov = box_bytes(b"moov", &mvhd);
        let mut buf = box_bytes(b"ftyp", b"iso5\x00\x00\x00\x00iso5").into_iter().chain(moov).collect::<Vec<u8>>();
        assert!(!patch_init_duration(&mut buf, 10.0));
    }

    #[test]
    fn boxes_with_size_zero_do_not_cause_an_infinite_loop() {
        // Craft a moov that reports size 0 (to end of file). We shouldn't hang.
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_be_bytes()); // size 0 = to EOF
        buf.extend_from_slice(b"moov");
        buf.extend_from_slice(&[0u8; 8]); // some bytes that would be a mvhd header start but incomplete
        let _ = patch_init_duration(&mut buf, 1.0);
    }
}
