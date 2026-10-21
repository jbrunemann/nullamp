//! The waveforms Mixxx analysed. Mixxx keeps each as a file named by its `track_analysis` row id in
//! the `analysis` folder beside the database: Qt's `qCompress` (the length as a big-endian u32, then
//! a zlib stream) around a protobuf `Waveform`. Only the fields used here are read:
//!
//! ```text
//! Waveform { double visual_sample_rate = 1; double audio_visual_ratio = 2;
//!            Signal all = 3; Filtered filtered = 4; }
//! Signal   { repeated int32 data = 1; }     // left and right interleaved, 0 to 255
//! Filtered { Signal low = 1; Signal mid = 2; Signal high = 3; }
//! ```

use std::io::Read;
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::Error;

/// A waveform as Mixxx drew it: per visual sample (left and right folded into the louder of the
/// two), the height of the whole signal and of its low, mid and high bands, each 0 to 255.
#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
    pub heights: Vec<u8>,
    /// Low, mid and high band, as long as `heights`; `None` when Mixxx stored no bands.
    pub bands: Option<[Vec<u8>; 3]>,
}

/// The longest analysis file read. The detail waveform of a two-hour mix is about 60 MB unpacked.
const MAX_UNPACKED: u64 = 256 << 20;

impl Waveform {
    /// `n` buckets (fewer if there are fewer samples) of peak and RMS, 0 to 255, the form of
    /// `nullamp_audio::Waveform`. Mixxx stores the heights as analysed and normalises them when it
    /// draws the overview, so the loudest sample is scaled to full here too.
    pub fn buckets(&self, n: usize) -> (Vec<u8>, Vec<u8>) {
        let len = self.heights.len();
        let n = n.min(len);
        let gain = 255.0 / f32::from(self.heights.iter().copied().max().unwrap_or(0).max(1));
        let (mut peak, mut rms) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for i in 0..n {
            let part = &self.heights[i * len / n..(i + 1) * len / n];
            let mean_square = part.iter().map(|&h| f32::from(h) * f32::from(h)).sum::<f32>() / part.len().max(1) as f32;
            peak.push((f32::from(part.iter().copied().max().unwrap_or(0)) * gain).round().min(255.0) as u8);
            rms.push((mean_square.sqrt() * gain).round().min(255.0) as u8);
        }
        (peak, rms)
    }

    /// One colour per bucket of [`buckets`](Self::buckets) with the same `n`, `None` without bands.
    /// Mixxx's RGB waveform draws low sounds red, mids green and highs blue; a bucket takes the
    /// average of its samples weighted by their height, scaled so its brightest channel is full.
    pub fn bucket_colors(&self, n: usize) -> Option<Vec<[u8; 3]>> {
        let bands = self.bands.as_ref()?;
        let len = self.heights.len();
        let n = n.min(len);
        Some(
            (0..n)
                .map(|i| {
                    let range = i * len / n..(i + 1) * len / n;
                    let mut sum = [0.0f32; 3];
                    for j in range {
                        let h = f32::from(self.heights[j]);
                        for (k, band) in bands.iter().enumerate() {
                            sum[k] += f32::from(band[j]) * h;
                        }
                    }
                    let brightest = sum.iter().copied().fold(0.0, f32::max);
                    if brightest == 0.0 {
                        return [128; 3];
                    }
                    sum.map(|s| (s / brightest * 255.0).round() as u8)
                })
                .collect(),
        )
    }
}

/// Reads the analysis file `path`. `Ok(None)` when the file is not there or holds no signal.
pub(crate) fn read(path: &Path) -> Result<Option<Waveform>, Error> {
    let packed = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let body = packed.get(4..).ok_or(Error::Format("analysis file"))?;
    let mut unpacked = Vec::new();
    ZlibDecoder::new(body).take(MAX_UNPACKED).read_to_end(&mut unpacked).map_err(|_| Error::Format("analysis file"))?;
    parse(&unpacked)
}

fn parse(bytes: &[u8]) -> Result<Option<Waveform>, Error> {
    let (mut all, mut filtered) = (None, None);
    for (field, value) in Fields(bytes) {
        match (field, value?) {
            (3, Value::Bytes(b)) => all = Some(signal(b)?),
            (4, Value::Bytes(b)) => filtered = Some(b),
            _ => {}
        }
    }
    let Some(all) = all.filter(|s| !s.is_empty()) else { return Ok(None) };
    let heights = fold_channels(&all);
    let bands = match filtered {
        Some(b) => {
            let mut bands: [Option<Vec<u8>>; 3] = [None, None, None];
            for (field, value) in Fields(b) {
                if let (1..=3, Value::Bytes(s)) = (field, value?) {
                    bands[field as usize - 1] = Some(fold_channels(&signal(s)?));
                }
            }
            match bands {
                [Some(low), Some(mid), Some(high)] if [&low, &mid, &high].iter().all(|b| b.len() == heights.len()) => Some([low, mid, high]),
                _ => None,
            }
        }
        None => None,
    };
    Ok(Some(Waveform { heights, bands }))
}

/// Left and right are stored one after the other for each visual sample.
fn fold_channels(data: &[u8]) -> Vec<u8> {
    data.chunks(2).map(|c| c.iter().copied().max().unwrap_or(0)).collect()
}

/// The `data` of a `Signal`, as bytes (a value above 255 is clamped).
fn signal(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut data = Vec::new();
    for (field, value) in Fields(bytes) {
        match (field, value?) {
            (1, Value::Varint(v)) => data.push(v.min(255) as u8),
            // The packed form, in case a Mixxx version writes it.
            (1, Value::Bytes(mut b)) => {
                while !b.is_empty() {
                    let (v, rest) = varint(b).ok_or(Error::Format("analysis file"))?;
                    data.push(v.min(255) as u8);
                    b = rest;
                }
            }
            _ => {}
        }
    }
    Ok(data)
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Other,
}

/// The fields of a protobuf message as (number, value); an item is an error once the message is cut off.
struct Fields<'a>(&'a [u8]);

fn varint(b: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (i, &byte) in b.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte < 0x80 {
            return Some((value, &b[i + 1..]));
        }
    }
    None
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u64, Result<Value<'a>, Error>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let bad = || Error::Format("analysis file");
        let Some((tag, rest)) = varint(self.0) else {
            self.0 = &[];
            return Some((0, Err(bad())));
        };
        let (field, wire) = (tag >> 3, tag & 7);
        let parsed = match wire {
            0 => varint(rest).map(|(v, r)| (Value::Varint(v), r)),
            1 => rest.get(8..).map(|r| (Value::Other, r)),
            5 => rest.get(4..).map(|r| (Value::Other, r)),
            2 => varint(rest).and_then(|(len, r)| {
                let len = usize::try_from(len).ok()?;
                Some((Value::Bytes(r.get(..len)?), &r[len..]))
            }),
            _ => None,
        };
        match parsed {
            Some((value, rest)) => {
                self.0 = rest;
                Some((field, Ok(value)))
            }
            None => {
                self.0 = &[];
                Some((field, Err(bad())))
            }
        }
    }
}
