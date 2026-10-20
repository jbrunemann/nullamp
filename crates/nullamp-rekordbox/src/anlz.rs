//! Waveforms from rekordbox analysis files (`ANLZ0000.DAT` and `.EXT`).
//!
//! A file is a `PMAI` header followed by sections, each starting with a four-letter tag and
//! its header and total length (big endian). Only the waveform sections are read; the beat
//! grid and cue sections of the same files are left for later.

use crate::Error;

/// Tallest column rekordbox stores: heights are 5 bits.
const MAX_HEIGHT: u32 = 31;

/// The track's loudness over time as rekordbox drew it, one column per sample point.
#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
    /// Column heights, 0 to 31. The `.EXT` detail waveform has 150 columns per second, the
    /// `.DAT` preview only 400 for the whole track.
    pub heights: Vec<u8>,
    /// Column colours (red, green, blue, each 0 to 7) when the file has the colour detail waveform.
    pub colors: Option<Vec<[u8; 3]>>,
}

impl Waveform {
    /// `n` buckets (fewer if there are fewer columns) of peak and RMS, 0 to 255, the form of
    /// `nullamp_audio::Waveform`.
    pub fn buckets(&self, n: usize) -> (Vec<u8>, Vec<u8>) {
        let len = self.heights.len();
        let n = n.min(len);
        let scale = |v: f32| (v * 255.0 / MAX_HEIGHT as f32).round().min(255.0) as u8;
        let (mut peak, mut rms) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for i in 0..n {
            let part = &self.heights[i * len / n..(i + 1) * len / n];
            let max = part.iter().copied().max().unwrap_or(0);
            let mean_square = part.iter().map(|&h| f32::from(h) * f32::from(h)).sum::<f32>() / part.len().max(1) as f32;
            peak.push(scale(f32::from(max)));
            rms.push(scale(mean_square.sqrt()));
        }
        (peak, rms)
    }
}

impl Waveform {
    /// One colour per bucket of [`buckets`](Self::buckets) with the same `n`, `None` without the
    /// colour waveform. rekordbox colours low sounds red and high ones blue; a bucket takes the
    /// average of its columns weighted by their height, scaled so its brightest channel is full.
    pub fn bucket_colors(&self, n: usize) -> Option<Vec<[u8; 3]>> {
        let colors = self.colors.as_ref()?;
        let len = colors.len().min(self.heights.len());
        let n = n.min(len);
        Some(
            (0..n)
                .map(|i| {
                    let range = i * len / n..(i + 1) * len / n;
                    let mut sum = [0.0f32; 3];
                    let mut weight = 0.0;
                    for (c, &h) in colors[range.clone()].iter().zip(&self.heights[range]) {
                        let h = f32::from(h);
                        weight += h;
                        for k in 0..3 {
                            sum[k] += f32::from(c[k]) * h;
                        }
                    }
                    let brightest = sum.iter().copied().fold(0.0, f32::max);
                    if weight == 0.0 || brightest == 0.0 {
                        return [128; 3];
                    }
                    sum.map(|s| (s / brightest * 255.0).round() as u8)
                })
                .collect(),
        )
    }
}

/// Body of the first section with `tag`, without its header.
pub(crate) fn section<'a>(data: &'a [u8], tag: &[u8; 4]) -> Result<Option<&'a [u8]>, Error> {
    let bad = Error::Format("analysis file");
    let be = |o: usize| data.get(o..o + 4).and_then(|b| b.try_into().ok()).map(u32::from_be_bytes);
    if data.get(..4) != Some(b"PMAI") {
        return Err(bad);
    }
    let mut at = be(4).ok_or(Error::Format("analysis file"))? as usize;
    while at + 12 <= data.len() {
        let (header, total) = (be(at + 4).ok_or(Error::Format("analysis file"))? as usize, be(at + 8).ok_or(Error::Format("analysis file"))? as usize);
        if total < header || at + total > data.len() {
            return Err(bad);
        }
        if &data[at..at + 4] == tag {
            return Ok(Some(&data[at + header..at + total]));
        }
        at += total;
    }
    Ok(None)
}

/// Heights of the waveform in `ext` (the `.EXT` file), with colours when it has them:
/// `PWV3` is one byte per column, `PWV5` two (red, green and blue 3 bits each, height 5).
pub fn detail(ext: &[u8]) -> Result<Option<Waveform>, Error> {
    if let Some(body) = section(ext, b"PWV5")? {
        let columns: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        let heights = columns.iter().map(|&c| ((c >> 2) & 0x1f) as u8).collect();
        let colors = columns.iter().map(|&c| [((c >> 13) & 7) as u8, ((c >> 10) & 7) as u8, ((c >> 7) & 7) as u8]).collect();
        return Ok(Some(Waveform { heights, colors: Some(colors) }));
    }
    Ok(section(ext, b"PWV3")?.map(|body| Waveform { heights: body.iter().map(|&b| b & 0x1f).collect(), colors: None }))
}

/// The 400-column preview in `dat` (the `.DAT` file), for sticks without `.EXT` files.
pub fn preview(dat: &[u8]) -> Result<Option<Waveform>, Error> {
    Ok(section(dat, b"PWAV")?.map(|body| Waveform { heights: body.iter().map(|&b| b & 0x1f).collect(), colors: None }))
}
