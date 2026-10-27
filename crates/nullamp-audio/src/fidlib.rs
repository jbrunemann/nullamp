//! Bessel filters designed and run the way Mixxx's waveform analysis does.
//!
//! A port of the parts of fidlib 0.9.10 (`fidlib.c`, `fidmkf.h`) that `EngineFilterBessel4Low`,
//! `EngineFilterBessel4Band` and `EngineFilterBessel4High` use, and of the way
//! `EngineFilterIIR` runs the result (a gain, then second-order stages in direct form II), from
//! the Mixxx sources (https://github.com/mixxxdj/mixxx, GPL-2.0-or-later).
//!
//! fidlib is Copyright (c) 2002-2004 Jim Peters <http://uazu.net/> and released under the GNU
//! Lesser General Public License version 2.1. Its filter design code is derived from `mkfilter` by
//! Tony Fisher of the University of York, who died in 2000; the University allowed its use on the
//! condition that the author's work is acknowledged and not used for financial profit.
//!
//! Only the 4th-order Bessel low-pass, high-pass and band-pass with the bilinear transform are
//! here. The band-pass doubles the order to 8, as in the original. Everything is `f64`.

use std::f64::consts::PI;

/// Poles with a positive imaginary part of a 4th-order Bessel low-pass whose -3 dB point is at 1 rad/s.
const BESSEL_4: [(f64, f64); 2] = [(-9.95208764350e-01, 1.25710573945e+00), (-1.37006783055e+00, 4.10249717494e-01)];

/// A complex number, only as much as the design needs.
#[derive(Clone, Copy, Debug)]
struct C(f64, f64);

impl C {
    fn add(self, o: C) -> C {
        C(self.0 + o.0, self.1 + o.1)
    }

    fn sub(self, o: C) -> C {
        C(self.0 - o.0, self.1 - o.1)
    }

    fn mul(self, o: C) -> C {
        C(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }

    fn div(self, o: C) -> C {
        let d = o.0 * o.0 + o.1 * o.1;
        C((self.0 * o.0 + self.1 * o.1) / d, (self.1 * o.0 - self.0 * o.1) / d)
    }

    fn scale(self, f: f64) -> C {
        C(self.0 * f, self.1 * f)
    }

    fn recip(self) -> C {
        C(1.0, 0.0).div(self)
    }

    /// The square root with a non-negative real part; tiny negative rounding errors count as zero.
    fn sqrt(self) -> C {
        let positive = |v: f64| if v <= 0.0 { 0.0 } else { v.sqrt() };
        let mag = self.0.hypot(self.1);
        let (re, im) = (positive((mag + self.0) * 0.5), positive((mag - self.0) * 0.5));
        C(re, if self.1 < 0.0 { -im } else { im })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    LowPass,
    HighPass,
    /// Between two corners; the filter has twice the order.
    BandPass,
}

/// One second-order stage: a numerator and a denominator in `z^-1`, each starting with 1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stage {
    fir: [f64; 3],
    iir: [f64; 3],
}

/// A designed filter: the gain and the stages in series.
#[derive(Clone, Debug)]
pub(crate) struct Design {
    gain: f64,
    stages: Vec<Stage>,
}

/// The pre-warped corner as fidlib does, `f` being a share of the sampling rate.
fn prewarp(f: f64) -> f64 {
    (f * PI).tan() / PI
}

impl Design {
    /// A 4th-order Bessel filter of `kind` at `hz` (and `hz_high` for a band-pass) for `rate`,
    /// with its response at 1 in the middle of the pass band: at 0 Hz for a low-pass, at half the
    /// rate for a high-pass, at the peak between the corners for a band-pass.
    pub fn bessel4(kind: Kind, rate: f64, hz: f64, hz_high: f64) -> Design {
        let (f0, f1) = (hz / rate, hz_high / rate);
        let mut poles: Vec<C> = BESSEL_4.iter().map(|&(re, im)| C(re, im)).collect();
        // Zeros as the real values they start from: at -infinity (z = -1) or at 0 (z = 1) in the s plane.
        let zeros_at_zero: usize;
        match kind {
            Kind::LowPass => {
                let w = 2.0 * PI * prewarp(f0);
                poles.iter_mut().for_each(|p| *p = p.scale(w));
                zeros_at_zero = 0;
            }
            Kind::HighPass => {
                let w = 2.0 * PI * prewarp(f0);
                poles.iter_mut().for_each(|p| *p = p.recip().scale(w));
                zeros_at_zero = poles.len() * 2;
            }
            Kind::BandPass => {
                let (p0, p1) = (prewarp(f0), prewarp(f1));
                let w0 = 2.0 * PI * (p0 * p1).sqrt();
                let bw = 0.5 * 2.0 * PI * (p1 - p0);
                // Each pole becomes two: hba * (1 +- sqrt(1 - (w0 / hba)^2)), hba being the pole times bw.
                poles = poles
                    .iter()
                    .flat_map(|p| {
                        let hba = p.scale(bw);
                        let temp = C(1.0, 0.0).sub(C(w0, 0.0).div(hba).mul(C(w0, 0.0).div(hba))).sqrt();
                        [hba.add(temp.mul(hba)), hba.sub(temp.mul(hba))]
                    })
                    .collect();
                zeros_at_zero = poles.len();
            }
        }
        let n_zeros = poles.len() * 2;
        // Bilinear transform: (2 + s) / (2 - s), which sends -infinity to -1 and 0 to 1.
        let poles: Vec<C> = poles.iter().map(|&p| C(2.0, 0.0).add(p).div(C(2.0, 0.0).sub(p))).collect();
        let zeros: Vec<f64> = (0..n_zeros).map(|i| if i < zeros_at_zero { 1.0 } else { -1.0 }).collect();

        let stages = poles
            .iter()
            .zip(zeros.chunks(2))
            .map(|(p, z)| Stage { iir: [1.0, -2.0 * p.0, p.0 * p.0 + p.1 * p.1], fir: [1.0, -(z[0] + z[1]), z[0] * z[1]] })
            .collect();
        let mut design = Design { gain: 1.0, stages };
        let reference = match kind {
            Kind::LowPass => 0.0,
            Kind::HighPass => 0.5,
            Kind::BandPass => design.search_peak(f0, f1),
        };
        design.gain = 1.0 / design.response(reference);
        design
    }

    /// The size of the response at `freq`, a share of the sampling rate from 0 to 0.5.
    fn response(&self, freq: f64) -> f64 {
        let theta = freq * 2.0 * PI;
        let z = C(theta.cos(), -theta.sin());
        let poly = |c: &[f64; 3]| C(c[0], 0.0).add(z.scale(c[1])).add(z.mul(z).scale(c[2]));
        let (mut top, mut bottom) = (C(self.gain, 0.0), C(1.0, 0.0));
        for stage in &self.stages {
            top = top.mul(poly(&stage.fir));
            bottom = bottom.mul(poly(&stage.iir));
        }
        let r = top.div(bottom);
        r.0.hypot(r.1)
    }

    /// The frequency of the peak between `f0` and `f3`, found by narrowing the range twenty times
    /// as fidlib does, assuming the response rises to one peak and falls again.
    fn search_peak(&self, mut f0: f64, mut f3: f64) -> f64 {
        for _ in 0..20 {
            let f1 = 0.51 * f0 + 0.49 * f3;
            let f2 = 0.49 * f0 + 0.51 * f3;
            if f1 == f2 {
                break;
            }
            if self.response(f1) > self.response(f2) {
                f3 = f2;
            } else {
                f0 = f1;
            }
        }
        (f0 + f3) * 0.5
    }

    /// The filter ready to run on one channel.
    pub fn runner(&self) -> Runner {
        Runner { gain: self.gain, stages: self.stages.clone(), state: vec![[0.0; 2]; self.stages.len()] }
    }
}

/// A filter with its history, run sample by sample in direct form II as Mixxx's `processSample` does.
#[derive(Clone)]
pub(crate) struct Runner {
    gain: f64,
    stages: Vec<Stage>,
    state: Vec<[f64; 2]>,
}

impl Runner {
    pub fn run(&mut self, x: f64) -> f64 {
        let mut value = x * self.gain;
        for (stage, w) in self.stages.iter().zip(&mut self.state) {
            let now = value - stage.iir[1] * w[0] - stage.iir[2] * w[1];
            value = now + stage.fir[1] * w[0] + stage.fir[2] * w[1];
            *w = [now, w[0]];
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `fid_design_coef` of the original fidlib 0.9.10 (the copy in Mixxx) gives, compiled and run:
    /// the gain, then for each stage the second and the first coefficient of its denominator.
    const REFERENCE: [(Kind, f64, f64, f64, f64, &[f64]); 9] = [
        (Kind::LowPass, 44100.0, 600.0, 0.0, 1.4401468432262392e-05, &[0.84377789118763691, -1.8265188696167201, 0.79090229871317685, -1.7775513973327368]),
        (Kind::HighPass, 44100.0, 4000.0, 0.0, 0.55323391341775108, &[0.6400545449474786, -1.5341105773730297, 0.45283439840333772, -1.3358493258425066]),
        (Kind::BandPass, 44100.0, 600.0, 4000.0, 0.0065947242259600967, &[0.43213024493118923, -1.0178913156834462, 0.93364369891705778, -1.9300570886190613, 0.28672667482191155, -1.0364006507689758, 0.86690809139950498, -1.8614774698570378]),
        (Kind::LowPass, 48000.0, 600.0, 0.0, 1.0423910743090728e-05, &[0.8554689985619921, -1.8408003273079265, 0.80616041400178795, -1.7947904290180952]),
        (Kind::HighPass, 48000.0, 4000.0, 0.0, 0.58064902013354958, &[0.66413700236118245, -1.5737095240183896, 0.48501371009474203, -1.3842959518813993]),
        (Kind::BandPass, 48000.0, 600.0, 4000.0, 0.0050519686322981596, &[0.45865555439393213, -1.0974356505197063, 0.93886706772826978, -1.9358281877176378, 0.32046578632164385, -1.1025397108954149, 0.87692233711485545, -1.8723021668934106]),
        (Kind::LowPass, 96000.0, 600.0, 0.0, 7.1289715815339242e-07, &[0.92483876276665034, -1.9210261194395193, 0.89795287473999952, -1.8949611565141466]),
        (Kind::HighPass, 96000.0, 4000.0, 0.0, 0.76126304349929652, &[0.81611832882574531, -1.7917933006127491, 0.70228978620535409, -1.6736822294629929]),
        (Kind::BandPass, 96000.0, 600.0, 4000.0, 0.00049045244949208987, &[0.66613944997486951, -1.5566845637408302, 0.96895264802669057, -1.9681778101174696, 0.57701590949269399, -1.5106453265290614, 0.93621596616409131, -1.93501222696539]),
    ];

    #[test]
    fn designs_match_the_original_fidlib() {
        for (kind, rate, f0, f1, gain, coefs) in REFERENCE {
            let design = Design::bessel4(kind, rate, f0, f1);
            let close = |a: f64, b: f64| (a - b).abs() <= 1e-12 * b.abs().max(1e-3);
            assert!(close(design.gain, gain), "{kind:?} at {rate}: gain {} against {gain}", design.gain);
            let ours: Vec<f64> = design.stages.iter().flat_map(|s| [s.iir[2], s.iir[1]]).collect();
            assert_eq!(ours.len(), coefs.len());
            for (a, b) in ours.iter().zip(coefs) {
                assert!(close(*a, *b), "{kind:?} at {rate}: {ours:?} against {coefs:?}");
            }
        }
    }

    #[test]
    fn numerators_are_the_constant_ones() {
        let band = Design::bessel4(Kind::BandPass, 44100.0, 600.0, 4000.0);
        let fir: Vec<[f64; 3]> = band.stages.iter().map(|s| s.fir).collect();
        assert_eq!(fir, [[1.0, -2.0, 1.0], [1.0, -2.0, 1.0], [1.0, 2.0, 1.0], [1.0, 2.0, 1.0]]);
        assert_eq!(Design::bessel4(Kind::LowPass, 44100.0, 600.0, 0.0).stages[0].fir, [1.0, 2.0, 1.0]);
        assert_eq!(Design::bessel4(Kind::HighPass, 44100.0, 4000.0, 0.0).stages[0].fir, [1.0, -2.0, 1.0]);
    }

    #[test]
    fn the_reference_points_are_at_unit_gain() {
        let low = Design::bessel4(Kind::LowPass, 44100.0, 600.0, 0.0);
        let high = Design::bessel4(Kind::HighPass, 44100.0, 4000.0, 0.0);
        let band = Design::bessel4(Kind::BandPass, 44100.0, 600.0, 4000.0);
        assert!((low.response(0.0) - 1.0).abs() < 1e-9);
        assert!((high.response(0.5) - 1.0).abs() < 1e-9);
        let peak = band.search_peak(600.0 / 44100.0, 4000.0 / 44100.0);
        assert!((band.response(peak) - 1.0).abs() < 1e-9);
        // Bessel -3 dB points at the corners.
        assert!((low.response(600.0 / 44100.0) - 0.7071).abs() < 0.01);
        assert!((high.response(4000.0 / 44100.0) - 0.7071).abs() < 0.01);
    }

    #[test]
    fn a_runner_follows_the_response() {
        let design = Design::bessel4(Kind::BandPass, 44100.0, 600.0, 4000.0);
        for hz in [100.0, 1500.0, 10000.0] {
            let mut run = design.runner();
            let out: Vec<f64> = (0..44100).map(|i| run.run((std::f64::consts::TAU * hz * f64::from(i) / 44100.0).sin())).collect();
            let rms = (out[22050..].iter().map(|x| x * x).sum::<f64>() / 22050.0).sqrt() * 2f64.sqrt();
            let expected = design.response(hz / 44100.0);
            assert!((rms - expected).abs() < 0.01 * expected.max(0.1), "{hz} Hz: {rms} against {expected}");
        }
    }
}
