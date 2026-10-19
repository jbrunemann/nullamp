//! File decoding into interleaved f32 at the output device's rate and channel count.
//!
//! Runs on the engine thread only. Nothing here is real-time sensitive.

use std::fs::File;
use std::path::{Path, PathBuf};

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

use crate::{LoadError, StreamInfo};

/// Give up on a file after this many consecutive undecodable packets.
const MAX_CONSECUTIVE_DECODE_ERRORS: u32 = 64;
/// Resampler input chunk in frames.
const RESAMPLE_CHUNK: usize = 1024;

pub(crate) enum Produced {
    /// New samples are available in `ready`.
    Data,
    /// The end of the stream was reached and everything has been flushed to `ready`.
    Eof,
}

pub(crate) struct Decoding {
    pub token: u64,
    pub path: PathBuf,
    pub info: StreamInfo,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    src_rate: u32,
    out_channels: usize,
    resampler: Option<Resample>,
    decoded: Vec<f32>,
    mapped: Vec<f32>,
    /// Output samples (interleaved, device layout) not yet pushed to the ring buffer.
    pub ready: Vec<f32>,
    pub ready_pos: usize,
    /// After a seek, frames before this time (seconds) are decoded and dropped so the
    /// seek lands exactly on the requested position.
    skip_until: Option<f64>,
    pub eof: bool,
    consecutive_errors: u32,
    /// Approximate position of the decoder (start of the last decoded packet), seconds.
    decoder_pos: f64,
    /// MP3 seeking is linear in symphonia's accurate mode; see [`Decoding::seek`].
    is_mp3: bool,
}

impl Decoding {
    pub fn open(token: u64, path: &Path, out_rate: u32, out_channels: usize) -> Result<Self, LoadError> {
        Self::open_as(token, path, Some((out_rate, out_channels)))
    }

    /// Open `path` for analysis: output keeps the file's own rate and channel layout, so
    /// nothing is resampled or remapped.
    pub fn open_native(path: &Path) -> Result<Self, LoadError> {
        Self::open_as(0, path, None)
    }

    /// `target` is the output (rate, channels); `None` keeps the file's own.
    fn open_as(token: u64, path: &Path, target: Option<(u32, usize)>) -> Result<Self, LoadError> {
        let file = File::open(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LoadError::NotFound,
            _ => LoadError::Io(e.to_string()),
        })?;
        let byte_len = file.metadata().map(|m| m.len()).ok();
        // Larger buffer than the default: files may live on network volumes.
        let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions { buffer_len: 256 * 1024 });

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| LoadError::Unsupported(describe(&e)))?;

        let track = format
            .default_track(TrackType::Audio)
            .or_else(|| format.first_track_known_codec(TrackType::Audio))
            .ok_or_else(|| LoadError::Unsupported("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| LoadError::Unsupported("no audio codec parameters".into()))?
            .clone();
        let track_id = track.id;
        let time_base = track.time_base;
        let duration = match (track.duration, time_base) {
            (Some(d), Some(tb)) => tb.calc_duration(d).map(|t| t.as_secs_f64()),
            _ => None,
        }
        .or_else(|| match (track.num_frames, params.sample_rate) {
            (Some(n), Some(r)) if r > 0 => Some(n as f64 / r as f64),
            _ => None,
        });

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| LoadError::Unsupported(describe(&e)))?;
        let is_mp3 = params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_MP3;
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map(|d| d.codec.info.short_name.to_uppercase())
            .unwrap_or_else(|| "?".into());

        let src_rate = params.sample_rate.ok_or_else(|| LoadError::Unsupported("unknown sample rate".into()))?;
        let src_channels = params.channels.as_ref().map(|c| c.count()).unwrap_or(2);
        let info = StreamInfo {
            duration,
            sample_rate: src_rate,
            channels: src_channels as u16,
            codec,
            bitrate: match (byte_len, duration) {
                (Some(b), Some(d)) if d > 1.0 => Some((b as f64 * 8.0 / d / 1000.0).round() as u32),
                _ => None,
            },
        };

        let (out_rate, out_channels) = target.unwrap_or((src_rate, src_channels));
        let resampler = if src_rate != out_rate {
            Some(Resample::new(src_rate, out_rate, out_channels).map_err(LoadError::Unsupported)?)
        } else {
            None
        };

        Ok(Decoding {
            token,
            path: path.to_path_buf(),
            info,
            format,
            decoder,
            track_id,
            time_base,
            src_rate,
            out_channels,
            resampler,
            decoded: Vec::new(),
            mapped: Vec::new(),
            ready: Vec::new(),
            ready_pos: 0,
            skip_until: None,
            eof: false,
            consecutive_errors: 0,
            decoder_pos: 0.0,
            is_mp3,
        })
    }

    /// Seek to `secs`. Buffered output is discarded. Returns the position that playback
    /// will resume from (equal to the request unless it was clamped).
    ///
    /// Symphonia's accurate MP3 seek parses every frame header between the current
    /// position and the target (and rescans from the start for backward seeks). That is
    /// fine for short hops but reads hundreds of megabytes for a far seek in a long mix,
    /// which is slow on network volumes. Far MP3 seeks therefore use the coarse mode,
    /// which jumps by byte position: exact for constant-bitrate files (the norm for DJ
    /// mixes), approximate for VBR files without a seek table. Decoding then continues
    /// from there and frames before the target are dropped.
    pub fn seek(&mut self, secs: f64) -> Result<f64, String> {
        let target = match self.info.duration {
            Some(d) => secs.clamp(0.0, (d - 0.05).max(0.0)),
            None => secs.max(0.0),
        };
        let time = Time::try_from_secs_f64(target).ok_or("invalid seek time")?;
        self.eof = false;
        let forward = target - self.decoder_pos;
        let mode = if self.is_mp3 && !(0.0..=20.0).contains(&forward) { SeekMode::Coarse } else { SeekMode::Accurate };
        match self.format.seek(mode, SeekTo::Time { time, track_id: Some(self.track_id) }) {
            Ok(_) => {}
            // Seeking past the end: treat as end of stream.
            Err(SymError::SeekError(_)) if self.info.duration.is_some_and(|d| target >= d - 0.5) => {
                self.eof = true;
            }
            Err(e) => return Err(describe(&e)),
        }
        self.decoder.reset();
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
        }
        self.ready.clear();
        self.ready_pos = 0;
        self.skip_until = Some(target);
        self.consecutive_errors = 0;
        Ok(target)
    }

    /// Decode until new output is produced or the stream ends.
    pub fn produce(&mut self) -> Result<Produced, String> {
        if self.ready_pos >= self.ready.len() {
            self.ready.clear();
            self.ready_pos = 0;
        }
        loop {
            if self.eof {
                if let Some(r) = self.resampler.as_mut() {
                    r.finish(&mut self.ready);
                }
                return Ok(Produced::Eof);
            }
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    continue;
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    continue;
                }
                Err(SymError::ResetRequired) => {
                    // Chained streams (e.g. concatenated Ogg). Treat as the end of this file.
                    self.eof = true;
                    continue;
                }
                Err(e) => return Err(describe(&e)),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let first_ts = packet.pts.get() + packet.trim_start.get() as i64;
            let start_secs = self.ts_to_secs(first_ts);
            self.decoder_pos = start_secs;
            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => {
                    self.consecutive_errors = 0;
                    buf
                }
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.consecutive_errors += 1;
                    if self.consecutive_errors > MAX_CONSECUTIVE_DECODE_ERRORS {
                        return Err("too many corrupt frames".into());
                    }
                    continue;
                }
                Err(e) => return Err(describe(&e)),
            };
            let src_channels = buf.spec().channels().count().max(1);
            let frames = buf.frames();
            if frames == 0 {
                continue;
            }
            buf.copy_to_vec_interleaved(&mut self.decoded);

            // Accurate seek: drop frames before the target.
            let mut skip_frames = 0usize;
            if let Some(target) = self.skip_until {
                let rate = buf.spec().rate().max(1) as f64;
                let end_secs = start_secs + frames as f64 / rate;
                if end_secs <= target {
                    continue;
                }
                skip_frames = (((target - start_secs) * rate).round().max(0.0) as usize).min(frames);
                self.skip_until = None;
            }
            let src = &self.decoded[skip_frames * src_channels..frames * src_channels];
            map_channels(src, src_channels, self.out_channels, &mut self.mapped);

            match self.resampler.as_mut() {
                Some(r) => r.push(&self.mapped, &mut self.ready),
                None => self.ready.extend_from_slice(&self.mapped),
            }
            if self.ready.len() > self.ready_pos {
                return Ok(Produced::Data);
            }
        }
    }

    fn ts_to_secs(&self, ts: i64) -> f64 {
        match self.time_base {
            Some(tb) => tb.calc_time(ts.into()).map(|t| t.as_secs_f64()).unwrap_or(0.0),
            None => ts as f64 / self.src_rate.max(1) as f64,
        }
    }
}

fn describe(e: &SymError) -> String {
    match e {
        SymError::Unsupported(what) => format!("unsupported: {what}"),
        SymError::IoError(io) => format!("read error: {io}"),
        other => other.to_string(),
    }
}

/// Map interleaved `src` with `src_ch` channels to `dst_ch` channels.
///
/// Mono is duplicated to the first two outputs; stereo and multichannel sources keep
/// their first two channels (front left/right); extra output channels are silent.
/// Stereo to mono averages.
pub(crate) fn map_channels(src: &[f32], src_ch: usize, dst_ch: usize, dst: &mut Vec<f32>) {
    dst.clear();
    if src_ch == dst_ch {
        dst.extend_from_slice(src);
        return;
    }
    let frames = src.len() / src_ch;
    dst.reserve(frames * dst_ch);
    for frame in src.chunks_exact(src_ch) {
        match (src_ch, dst_ch) {
            (_, 1) => dst.push(if src_ch >= 2 { 0.5 * (frame[0] + frame[1]) } else { frame[0] }),
            (1, _) => {
                dst.push(frame[0]);
                dst.push(frame[0]);
                dst.extend(std::iter::repeat_n(0.0, dst_ch - 2));
            }
            _ => {
                let copy = src_ch.min(dst_ch);
                if dst_ch >= 2 && src_ch > dst_ch {
                    dst.extend_from_slice(&frame[..dst_ch]);
                } else {
                    dst.extend_from_slice(&frame[..copy]);
                    dst.extend(std::iter::repeat_n(0.0, dst_ch - copy));
                }
            }
        }
    }
}

/// Fixed-ratio resampling with rubato's FFT resampler, fed in arbitrary-sized pieces.
pub(crate) struct Resample {
    inner: Fft<f32>,
    channels: usize,
    ratio: f64,
    input: Vec<f32>,
    out_buf: Vec<f32>,
    /// Output frames still to drop to compensate the resampler's delay.
    delay_left: usize,
    in_total: u64,
    out_total: u64,
}

impl Resample {
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self, String> {
        let inner = Fft::<f32>::new(from as usize, to as usize, RESAMPLE_CHUNK, channels, FixedSync::Input)
            .map_err(|e| format!("resampler: {e}"))?;
        let delay_left = inner.output_delay();
        let out_buf = vec![0.0; inner.output_frames_max() * channels];
        Ok(Resample {
            inner,
            channels,
            ratio: to as f64 / from as f64,
            input: Vec::with_capacity(RESAMPLE_CHUNK * channels * 2),
            out_buf,
            delay_left,
            in_total: 0,
            out_total: 0,
        })
    }

    pub fn reset(&mut self) {
        self.inner.reset();
        self.input.clear();
        self.delay_left = self.inner.output_delay();
        self.in_total = 0;
        self.out_total = 0;
    }

    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        self.in_total += (samples.len() / self.channels) as u64;
        self.input.extend_from_slice(samples);
        let mut consumed = 0;
        while (self.input.len() - consumed * self.channels) / self.channels >= self.inner.input_frames_next() {
            let (n_in, _) = self.process(consumed, None, out);
            consumed += n_in;
        }
        self.input.drain(..consumed * self.channels);
    }

    /// Flush the remaining input and the resampler's internal delay.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let expected = (self.in_total as f64 * self.ratio).round() as u64;
        let remaining = self.input.len() / self.channels;
        let mut guard = 0;
        let mut consumed = 0;
        while self.out_total < expected && guard < 8 {
            let left = remaining - consumed;
            let (n_in, _) = self.process(consumed, Some(left.min(self.inner.input_frames_next())), out);
            consumed += n_in.min(left);
            guard += 1;
        }
        // Drop padding beyond the expected length.
        let excess = self.out_total.saturating_sub(expected) as usize * self.channels;
        out.truncate(out.len().saturating_sub(excess));
        self.out_total = expected.min(self.out_total);
        self.input.clear();
    }

    fn process(&mut self, input_offset_frames: usize, partial: Option<usize>, out: &mut Vec<f32>) -> (usize, usize) {
        let in_frames = self.input.len() / self.channels;
        // A partial chunk still needs a buffer of the full chunk length.
        let need = self.inner.input_frames_next();
        let padded;
        let (buf, offset, frames): (&[f32], usize, usize) = if in_frames - input_offset_frames < need {
            let mut v = self.input[input_offset_frames * self.channels..].to_vec();
            v.resize(need * self.channels, 0.0);
            padded = v;
            (&padded, 0, need)
        } else {
            (&self.input, input_offset_frames, in_frames)
        };
        let input = InterleavedSlice::new(buf, self.channels, frames).expect("input adapter");
        let out_frames_cap = self.out_buf.len() / self.channels;
        let mut output = InterleavedSlice::new_mut(&mut self.out_buf, self.channels, out_frames_cap).expect("output adapter");
        let indexing = Indexing { input_offset: offset, output_offset: 0, partial_len: partial, active_channels_mask: None };
        let (n_in, n_out) = match self.inner.process_into_buffer(&input, &mut output, Some(&indexing)) {
            Ok(r) => r,
            Err(e) => {
                log::error!("resampler error: {e}");
                return (need, 0);
            }
        };
        let drop = self.delay_left.min(n_out);
        self.delay_left -= drop;
        out.extend_from_slice(&self.out_buf[drop * self.channels..n_out * self.channels]);
        self.out_total += (n_out - drop) as u64;
        (n_in, n_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_mapping() {
        let mut out = Vec::new();
        map_channels(&[0.1, 0.2], 1, 2, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2]);
        map_channels(&[0.2, 0.4], 2, 1, &mut out);
        assert!((out[0] - 0.3).abs() < 1e-6);
        map_channels(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, 2, &mut out);
        assert_eq!(out, [1.0, 2.0]);
        map_channels(&[1.0, 2.0], 2, 4, &mut out);
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0]);
        map_channels(&[1.0], 1, 4, &mut out);
        assert_eq!(out, [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn resampler_preserves_length_and_tone() {
        // 1 s of 1 kHz at 44.1 kHz, fed in odd-sized pieces, resampled to 48 kHz.
        let from = 44_100;
        let to = 48_000;
        let input: Vec<f32> = (0..from)
            .flat_map(|i| {
                let s = (i as f32 * 1000.0 * std::f32::consts::TAU / from as f32).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut r = Resample::new(from, to, 2).unwrap();
        let mut out = Vec::new();
        for piece in input.chunks(1152 * 2) {
            r.push(piece, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len() / 2, to as usize, "exact output length");
        // The signal should start immediately (delay compensated): sample 12 at 48k is
        // sin(2π·1000·12/48000) * 0.5 = 0.5 * sin(π/2) = 0.5.
        assert!((out[12 * 2] - 0.5).abs() < 0.02, "got {}", out[24]);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02);
    }
}
