//! Spectrum analyser: FFT over the tapped audio, grouped into log-spaced bars.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

use crate::spotify::sink::SampleTap;

const FFT_N: usize = 2048;
const SAMPLE_RATE: f32 = 44_100.0;
const F_MIN: f32 = 40.0;
const F_MAX: f32 = 16_000.0;
const DB_FLOOR: f32 = -70.0;
const DECAY: f32 = 0.82;

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    samples: Vec<f32>,
    buf: Vec<Complex<f32>>,
    bands: Vec<(usize, usize)>,
    /// Bar heights in 0.0..=1.0.
    pub bars: Vec<f32>,
}

impl Default for Spectrum {
    fn default() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_N);
        let window = (0..FFT_N)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / (FFT_N - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Self {
            fft,
            window,
            samples: Vec::with_capacity(FFT_N),
            buf: vec![Complex::default(); FFT_N],
            bands: Vec::new(),
            bars: Vec::new(),
        }
    }
}

impl Spectrum {
    fn resize(&mut self, n_bars: usize) {
        if self.bars.len() != n_bars {
            self.bars = vec![0.0; n_bars];
            self.bands = band_edges(n_bars, FFT_N, SAMPLE_RATE, F_MIN, F_MAX);
        }
    }

    /// Analyse the latest audio from the tap.
    pub fn update(&mut self, tap: &SampleTap, n_bars: usize) {
        self.resize(n_bars);
        if n_bars == 0 {
            return;
        }
        tap.latest(FFT_N, &mut self.samples);
        for (i, c) in self.buf.iter_mut().enumerate() {
            *c = Complex::new(self.samples[i] * self.window[i], 0.0);
        }
        self.fft.process(&mut self.buf);

        let norm = FFT_N as f32 / 4.0;
        for (bar, &(lo, hi)) in self.bars.iter_mut().zip(&self.bands) {
            let peak = self.buf[lo..hi]
                .iter()
                .map(|c| c.norm() / norm)
                .fold(0.0f32, f32::max);
            let db = 20.0 * peak.max(1e-9).log10();
            let level = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
            *bar = level.max(*bar * DECAY);
        }
    }

    /// Let the bars fall to zero (paused / stopped).
    pub fn decay(&mut self, n_bars: usize) {
        self.resize(n_bars);
        for bar in &mut self.bars {
            *bar *= DECAY;
        }
    }

    /// A gentle synthetic animation for when music plays on another device.
    pub fn idle(&mut self, t: f32, n_bars: usize) {
        self.resize(n_bars);
        for (i, bar) in self.bars.iter_mut().enumerate() {
            let x = i as f32 / n_bars.max(1) as f32;
            let wave = (t * 2.1 + x * 9.0).sin() * 0.5 + (t * 3.3 - x * 17.0).sin() * 0.3;
            let target = (0.25 + 0.2 * wave) * (1.0 - 0.6 * x);
            *bar = *bar * 0.8 + target.clamp(0.02, 1.0) * 0.2;
        }
    }
}

/// Splits the FFT bins between `f_min` and `f_max` into `n` log-spaced, non-empty,
/// non-overlapping ranges `[lo, hi)`.
pub fn band_edges(
    n: usize,
    fft_n: usize,
    sample_rate: f32,
    f_min: f32,
    f_max: f32,
) -> Vec<(usize, usize)> {
    let bin_hz = sample_rate / fft_n as f32;
    let max_bin = fft_n / 2;
    let mut edges = Vec::with_capacity(n);
    let mut prev = ((f_min / bin_hz).floor() as usize).max(1);
    for i in 1..=n {
        let f = f_min * (f_max / f_min).powf(i as f32 / n as f32);
        let mut hi = ((f / bin_hz).round() as usize).min(max_bin);
        if hi <= prev {
            hi = (prev + 1).min(max_bin);
        }
        let lo = prev.min(hi.saturating_sub(1));
        edges.push((lo, hi));
        prev = hi;
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_ordered_and_non_empty() {
        for n in [1, 8, 32, 64, 120] {
            let bands = band_edges(n, FFT_N, SAMPLE_RATE, F_MIN, F_MAX);
            assert_eq!(bands.len(), n);
            for w in bands.windows(2) {
                assert!(w[0].1 <= w[1].0 + 1);
            }
            for &(lo, hi) in &bands {
                assert!(lo < hi && hi <= FFT_N / 2);
            }
        }
    }

    #[test]
    fn sine_lights_up_the_right_band() {
        let tap = SampleTap::default();
        let stereo: Vec<f64> = (0..FFT_N)
            .flat_map(|i| {
                let s = (std::f64::consts::TAU * 1000.0 * i as f64 / 44_100.0).sin() * 0.8;
                [s, s]
            })
            .collect();
        // Feed through the public tap API via a sink-like path.
        crate::spotify::sink::tests_support::push(&tap, &stereo);
        let mut spec = Spectrum::default();
        spec.update(&tap, 32);
        let loudest = spec
            .bars
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        let (lo, hi) = spec.bands[loudest];
        let bin_hz = SAMPLE_RATE / FFT_N as f32;
        assert!((lo as f32 * bin_hz) <= 1100.0 && (hi as f32 * bin_hz) >= 900.0);
    }
}
