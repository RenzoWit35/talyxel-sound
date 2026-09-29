//! Audio sink wrapper that forwards to the real backend and taps the samples for the visualizer.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use librespot_playback::{
    audio_backend::{Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

/// Number of mono samples kept for the spectrum analyser.
pub const TAP_LEN: usize = 4096;

/// Shared buffer holding the most recently written mono samples.
#[derive(Clone, Default)]
pub struct SampleTap(Arc<Mutex<VecDeque<f32>>>);

impl SampleTap {
    fn push_interleaved_stereo(&self, samples: &[f64]) {
        let Ok(mut buf) = self.0.lock() else { return };
        for frame in samples.as_chunks::<2>().0 {
            buf.push_back(((frame[0] + frame[1]) * 0.5) as f32);
        }
        let excess = buf.len().saturating_sub(TAP_LEN);
        buf.drain(..excess);
    }

    /// Copies the latest `n` samples into `out` (zero-padded at the front if fewer exist).
    pub fn latest(&self, n: usize, out: &mut Vec<f32>) {
        out.clear();
        let Ok(buf) = self.0.lock() else { return };
        let have = buf.len().min(n);
        out.resize(n - have, 0.0);
        out.extend(buf.iter().skip(buf.len() - have));
    }

    pub fn clear(&self) {
        if let Ok(mut buf) = self.0.lock() {
            buf.clear();
        }
    }
}

pub struct TapSink {
    inner: Box<dyn Sink>,
    tap: SampleTap,
}

impl TapSink {
    pub fn new(inner: Box<dyn Sink>, tap: SampleTap) -> Self {
        Self { inner, tap }
    }
}

impl Sink for TapSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.tap.clear();
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let AudioPacket::Samples(samples) = &packet {
            self.tap.push_interleaved_stereo(samples);
        }
        self.inner.write(packet, converter)
    }
}

#[cfg(test)]
pub mod tests_support {
    pub fn push(tap: &super::SampleTap, stereo: &[f64]) {
        tap.push_interleaved_stereo(stereo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_downmixes_and_caps_length() {
        let tap = SampleTap::default();
        let stereo: Vec<f64> = (0..(TAP_LEN * 2 + 100) * 2)
            .map(|i| (i % 2) as f64)
            .collect();
        tap.push_interleaved_stereo(&stereo);
        let mut out = Vec::new();
        tap.latest(TAP_LEN * 2, &mut out);
        assert_eq!(out.len(), TAP_LEN * 2);
        assert!(out[..TAP_LEN].iter().all(|&s| s == 0.0));
        assert!(out[TAP_LEN..].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }
}
