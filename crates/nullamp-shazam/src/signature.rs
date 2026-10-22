//! Shazam's signature format: spectral peaks of the 16 kHz signal in four frequency bands,
//! behind a header with a CRC. Peak picking follows the apps exactly, since the server
//! compares the peaks themselves.

use realfft::RealFftPlanner;

use crate::RATE;

const FFT_SIZE: usize = 2048;
const HOP: usize = 128;
const BINS: usize = FFT_SIZE / 2 + 1;
/// Spectra kept: peaks are judged against spectra up to ~250 hops away.
const HISTORY: usize = 256;
/// A peak is judged 46 hops after its spectrum, once the spectra around it exist.
const DELAY: usize = 46;
/// Bins around a peak (in the spread spectrum three hops later) that it must exceed.
const FREQ_NEIGHBOURS: [isize; 8] = [-10, -7, -4, -3, 1, 2, 5, 8];
/// Spread spectra, relative to the newest, whose bin below the peak it must exceed.
const TIME_NEIGHBOURS: [isize; 14] = [-53, -45, 165, 172, 179, 186, 193, 200, 214, 221, 228, 235, 242, 249];
const MIN_POWER: f64 = 1.0 / 64.0;

struct Peak {
    pass: u32,
    magnitude: u16,
    /// FFT bin in 1/64 steps.
    bin: u16,
}

pub fn encode(samples: &[f32]) -> Vec<u8> {
    let mut contents = Vec::new();
    for (band, peaks) in peaks(samples).iter().enumerate().filter(|(_, p)| !p.is_empty()) {
        let mut body = Vec::new();
        let mut last = 0;
        for p in peaks {
            if p.pass - last >= 255 {
                body.push(0xff);
                body.extend(p.pass.to_le_bytes());
                last = p.pass;
            }
            body.push((p.pass - last) as u8);
            body.extend(p.magnitude.to_le_bytes());
            body.extend(p.bin.to_le_bytes());
            last = p.pass;
        }
        contents.extend((0x6003_0040 + band as u32).to_le_bytes());
        contents.extend((body.len() as u32).to_le_bytes());
        contents.extend(&body);
        contents.resize(contents.len() + (4 - body.len() % 4) % 4, 0);
    }

    let mut out = Vec::with_capacity(56 + contents.len());
    out.extend(0xcafe_2580u32.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // CRC, filled in below
    out.extend((contents.len() as u32 + 8).to_le_bytes());
    out.extend(0x9411_9c00u32.to_le_bytes());
    out.extend([0; 12]);
    out.extend((3u32 << 27).to_le_bytes()); // sample rate id 3: 16 kHz
    out.extend([0; 8]);
    out.extend((samples.len() as u32 + (RATE as f64 * 0.24) as u32).to_le_bytes());
    out.extend(((15u32 << 19) + 0x40000).to_le_bytes());
    out.extend(0x4000_0000u32.to_le_bytes());
    out.extend((contents.len() as u32 + 8).to_le_bytes());
    out.extend(&contents);
    let crc = crc32(&out[8..]);
    out[4..8].copy_from_slice(&crc.to_le_bytes());
    out
}

/// Peaks per band (250-520, 520-1450, 1450-3500 and 3500-5500 Hz), in time order.
fn peaks(samples: &[f32]) -> [Vec<Peak>; 4] {
    // numpy.hanning(2050)[1:-1], as the apps use.
    let window: Vec<f64> = (1..=FFT_SIZE).map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (FFT_SIZE + 1) as f64).cos()).collect();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(FFT_SIZE);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut ring = vec![0.0f64; FFT_SIZE];
    let mut ring_pos = 0;
    let mut powers = vec![vec![0.0f64; BINS]; HISTORY];
    let mut spread = vec![vec![0.0f64; BINS]; HISTORY];
    let mut written = 0usize;
    let mut bands: [Vec<Peak>; 4] = Default::default();
    let at = |written: usize, offset: isize| (written as isize + offset).rem_euclid(HISTORY as isize) as usize;

    for chunk in samples.chunks_exact(HOP) {
        for &s in chunk {
            ring[ring_pos] = (f64::from(s) * 32767.0).round().clamp(-32768.0, 32767.0);
            ring_pos = (ring_pos + 1) % FFT_SIZE;
        }
        for (i, x) in input.iter_mut().enumerate() {
            *x = ring[(ring_pos + i) % FFT_SIZE] * window[i];
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            continue;
        }
        let power: Vec<f64> = spectrum.iter().map(|c| (c.norm_sqr() / f64::from(1 << 17)).max(1e-10)).collect();

        // Spread each peak over its two upper neighbours and into the spectra 1, 3 and 6 hops back.
        let mut s = power.clone();
        for p in 0..BINS - 2 {
            s[p] = s[p].max(s[p + 1]).max(s[p + 2]);
        }
        for (p, &value) in s.iter().enumerate() {
            let mut max = value;
            for back in [1, 3, 6] {
                let former = &mut spread[at(written, -back)][p];
                max = max.max(*former);
                *former = max;
            }
        }
        powers[written % HISTORY] = power;
        spread[written % HISTORY] = s;
        written += 1;
        if written < DELAY {
            continue;
        }

        let candidate = &powers[at(written, -(DELAY as isize))];
        let around = &spread[at(written, -49)];
        for bin in 10..1015 {
            let v = candidate[bin];
            if v < MIN_POWER || v < around[bin - 1] {
                continue;
            }
            let mut max = FREQ_NEIGHBOURS.iter().map(|&o| around[(bin as isize + o) as usize]).fold(0.0, f64::max);
            if v <= max {
                continue;
            }
            for &o in &TIME_NEIGHBOURS {
                max = max.max(spread[at(written, o)][bin - 1]);
            }
            if v <= max {
                continue;
            }
            let magnitude = |x: f64| x.max(MIN_POWER).ln() * 1477.3 + 6144.0;
            let (m, below, above) = (magnitude(v), magnitude(candidate[bin - 1]), magnitude(candidate[bin + 1]));
            let curvature = m * 2.0 - below - above;
            if curvature <= 0.0 {
                continue;
            }
            let corrected = bin as f64 * 64.0 + (above - below) * 32.0 / curvature;
            let hz = corrected * (f64::from(RATE) / 2.0 / 1024.0 / 64.0);
            let band = match hz {
                h if h < 250.0 => continue,
                h if h < 520.0 => 0,
                h if h < 1450.0 => 1,
                h if h < 3500.0 => 2,
                h if h <= 5500.0 => 3,
                _ => continue,
            };
            bands[band].push(Peak { pass: (written - DELAY) as u32, magnitude: m as u16, bin: corrected as u16 });
        }
    }
    bands
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn crc_is_the_zlib_one() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn silence_has_a_header_and_no_peaks() {
        let sig = encode(&vec![0.0; 16000 * 3]);
        assert_eq!(sig.len(), 56);
        assert_eq!(u32_at(&sig, 0), 0xcafe_2580);
        assert_eq!(u32_at(&sig, 4), crc32(&sig[8..]));
        assert_eq!(u32_at(&sig, 8), 8);
        assert_eq!(u32_at(&sig, 28), 3 << 27);
        assert_eq!(u32_at(&sig, 40), 48000 + 3840);
    }

    #[test]
    fn notes_give_peaks_in_their_bands() {
        // A note every quarter second, alternating between 440 Hz and 2 kHz.
        let samples: Vec<f32> = (0..16000 * 4)
            .map(|i| {
                let t = i as f32 / 16000.0;
                let hz = if ((t * 4.0) as u32).is_multiple_of(2) { 440.0 } else { 2000.0 };
                let envelope = (std::f32::consts::PI * (t * 4.0).fract()).sin();
                0.5 * envelope * (2.0 * std::f32::consts::PI * hz * t).sin()
            })
            .collect();
        let bands = peaks(&samples);
        // Pure tones also leave sidelobe peaks elsewhere (music hides them), so only check
        // that each note shows up in its band, once per note, at its frequency.
        let hz = |p: &Peak| f64::from(p.bin) * f64::from(RATE) / 2.0 / 1024.0 / 64.0;
        let near = |band: &[Peak], f: f64| band.iter().filter(|p| (hz(p) - f).abs() < 2.0).count();
        assert!(near(&bands[0], 440.0) >= 7 && near(&bands[2], 2000.0) >= 7, "peaks per band: {:?}", bands.iter().map(Vec::len).collect::<Vec<_>>());
        assert!(bands.iter().all(|b| b.windows(2).all(|w| w[0].pass <= w[1].pass)));
        let sig = encode(&samples);
        assert_eq!(u32_at(&sig, 4), crc32(&sig[8..]));
        assert_eq!(u32_at(&sig, 8) as usize, sig.len() - 48);
    }
}
