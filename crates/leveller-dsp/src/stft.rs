//! Short-time Fourier transform with weighted overlap-add.
//!
//! Any stage that changes a spectrum frame by frame — the denoiser, the dynamic
//! EQ — has to get back to a waveform without seams. The requirement is
//! *perfect reconstruction*: with the spectrum left alone, analysis followed by
//! synthesis returns the input sample for sample, so anything heard afterwards
//! is the processing and not the transform.
//!
//! The arrangement is the standard one for modifying magnitudes: a square-root
//! Hann window on both analysis and synthesis, hopping a quarter of the frame.
//! Their product is a Hann window, and Hann at 75% overlap sums to a constant,
//! which the overlap-add divides straight back out. Windowing only on analysis
//! would be simpler and would ring — a modified frame no longer tapers to zero
//! at its edges, and that discontinuity lands in the output.
//!
//! [`Stft::process`] is the one to reach for. It holds a fixed-size overlap-add
//! buffer, so memory is O(frame_size) however long the file is; materialising
//! every frame instead costs 8 KB per frame at the usual 1024/256, which is two
//! gigabytes for a twenty-minute recording and a render spent swapping.

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex64;
use std::sync::Arc;

/// Square root of a periodic Hann window.
pub fn sqrt_hann_window(size: usize) -> Vec<f64> {
    crate::fft::hann_window(size)
        .into_iter()
        .map(f64::sqrt)
        .collect()
}

/// The transform's geometry: how long a frame is, and how far apart frames sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Stft {
    /// Frame length in samples. A power of two.
    pub frame_size: usize,
    /// Hop between frames. `frame_size / 4` gives the 75% overlap the
    /// weighted overlap-add wants.
    pub hop_size: usize,
}

impl Default for Stft {
    fn default() -> Self {
        Self {
            frame_size: 1024,
            hop_size: 256,
        }
    }
}

impl Stft {
    /// # Panics
    /// If the frame size is not a power of two, or not a whole number of hops.
    pub fn new(frame_size: usize, hop_size: usize) -> Self {
        let stft = Self {
            frame_size,
            hop_size,
        };
        stft.check();
        stft
    }

    fn check(&self) {
        assert!(
            self.frame_size.is_power_of_two(),
            "frame size must be a power of two"
        );
        assert!(self.hop_size > 0, "hop size must be positive");
        assert!(
            self.frame_size.is_multiple_of(self.hop_size),
            "frame size must be a whole number of hops"
        );
    }

    /// Bins a frame carries: DC through Nyquist, inclusive.
    pub fn bins(&self) -> usize {
        self.frame_size / 2 + 1
    }

    /// How many analysis frames a signal of `length` samples produces.
    ///
    /// Worth knowing before allocating anything: at 1024/256 a twenty-minute
    /// recording has a quarter of a million frames.
    pub fn frame_count(&self, length: usize) -> usize {
        // The signal is padded by a frame at each end, so the first and last
        // samples get the same overlap treatment as the middle. Without it a
        // denoiser's first 20 ms would be attenuated by the window taper alone.
        (length + self.frame_size) / self.hop_size + 1
    }

    /// Walk a signal's spectra without resynthesising, for a pass that only
    /// measures — a noise profile, a long-term average.
    pub fn scan(&self, samples: &[f32], mut visit: impl FnMut(&[Complex64], usize)) -> usize {
        self.check();
        let mut state = Analysis::new(*self);
        let frames = self.frame_count(samples.len());
        for f in 0..frames {
            state.analyse(samples, f);
            visit(&state.spectrum, f);
        }
        frames
    }

    /// Analyse, modify and resynthesise, one frame at a time.
    ///
    /// `transform` is handed each frame's spectrum to modify in place. The
    /// overlap-add state is a ring of exactly one frame: a sample stops
    /// receiving contributions once the frame starting at it has been added, so
    /// `hop_size` slots retire per frame and are immediately reusable.
    pub fn process(
        &self,
        samples: &[f32],
        mut transform: impl FnMut(&mut [Complex64], usize),
    ) -> Vec<f32> {
        self.check();
        let mut state = Analysis::new(*self);
        let mut synthesis = Synthesis::new(*self);
        let length = samples.len();
        let mut out = vec![0.0f32; length];

        for f in 0..self.frame_count(length) {
            state.analyse(samples, f);
            transform(&mut state.spectrum, f);
            synthesis.add(&mut state.spectrum, f);
            synthesis.retire(f, length, &mut out);
        }

        out
    }

    /// Every frame at once, for callers that genuinely need to look across the
    /// whole file before deciding anything.
    ///
    /// Prefer [`Stft::process`] where the decision is per-frame: this holds the
    /// entire spectrogram in memory.
    pub fn analyze(&self, samples: &[f32]) -> Frames {
        let mut frames = Vec::with_capacity(self.frame_count(samples.len()));
        self.scan(samples, |spectrum, _| frames.push(spectrum.to_vec()));
        Frames {
            stft: *self,
            frames,
            length: samples.len(),
        }
    }
}

/// A whole signal's spectra, and what it takes to put them back together.
#[derive(Clone, Debug)]
pub struct Frames {
    pub stft: Stft,
    /// Per frame, bins 0..=frame_size/2.
    pub frames: Vec<Vec<Complex64>>,
    /// Length of the signal that produced them.
    pub length: usize,
}

impl Frames {
    /// Rebuild the signal from (possibly modified) spectra.
    pub fn synthesize(&self) -> Vec<f32> {
        let mut synthesis = Synthesis::new(self.stft);
        let mut out = vec![0.0f32; self.length];
        let mut scratch = vec![Complex64::default(); self.stft.bins()];

        for (f, spectrum) in self.frames.iter().enumerate() {
            scratch.copy_from_slice(spectrum);
            synthesis.add(&mut scratch, f);
            synthesis.retire(f, self.length, &mut out);
        }
        // Frames after the last one contribute nothing, but samples they would
        // have overlapped still need emitting.
        for f in self.frames.len()..self.stft.frame_count(self.length) {
            synthesis.retire(f, self.length, &mut out);
        }
        out
    }
}

/// Windowing and the forward transform, against buffers that live across frames.
struct Analysis {
    stft: Stft,
    fft: Arc<dyn RealToComplex<f64>>,
    window: Vec<f64>,
    windowed: Vec<f64>,
    scratch: Vec<Complex64>,
    spectrum: Vec<Complex64>,
}

impl Analysis {
    fn new(stft: Stft) -> Self {
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(stft.frame_size);
        let scratch = fft.make_scratch_vec();
        Self {
            stft,
            fft,
            window: sqrt_hann_window(stft.frame_size),
            windowed: vec![0.0; stft.frame_size],
            scratch,
            spectrum: vec![Complex64::default(); stft.bins()],
        }
    }

    fn analyse(&mut self, samples: &[f32], frame: usize) {
        let start = frame * self.stft.hop_size;
        for i in 0..self.stft.frame_size {
            // `start` runs over the padded timeline; shift back to the signal.
            let at = (start + i) as isize - self.stft.frame_size as isize;
            let value = usize::try_from(at)
                .ok()
                .and_then(|at| samples.get(at))
                .copied()
                .unwrap_or(0.0);
            self.windowed[i] = f64::from(value) * self.window[i];
        }
        self.fft
            .process_with_scratch(&mut self.windowed, &mut self.spectrum, &mut self.scratch)
            .expect("buffers are the plan's own sizes");
    }
}

/// The inverse transform and the overlap-add ring.
struct Synthesis {
    stft: Stft,
    fft: Arc<dyn ComplexToReal<f64>>,
    window: Vec<f64>,
    frame: Vec<f64>,
    scratch: Vec<Complex64>,
    /// Overlap-add accumulator and its normalisation, indexed modulo the frame
    /// size.
    acc: Vec<f64>,
    nrm: Vec<f64>,
}

impl Synthesis {
    fn new(stft: Stft) -> Self {
        let fft = RealFftPlanner::<f64>::new().plan_fft_inverse(stft.frame_size);
        let scratch = fft.make_scratch_vec();
        Self {
            stft,
            fft,
            window: sqrt_hann_window(stft.frame_size),
            frame: vec![0.0; stft.frame_size],
            scratch,
            acc: vec![0.0; stft.frame_size],
            nrm: vec![0.0; stft.frame_size],
        }
    }

    /// Invert one frame and fold it into the ring.
    fn add(&mut self, spectrum: &mut [Complex64], frame: usize) {
        // DC and Nyquist are real for any real signal. A stage that scaled bins
        // by real gains keeps them so; one that wrote arbitrary complex values
        // did not, and the inverse transform would refuse the frame. Take the
        // real part rather than failing on it.
        if let Some(first) = spectrum.first_mut() {
            first.im = 0.0;
        }
        if let Some(last) = spectrum.last_mut() {
            last.im = 0.0;
        }
        self.fft
            .process_with_scratch(spectrum, &mut self.frame, &mut self.scratch)
            .expect("buffers are the plan's own sizes");

        let scale = 1.0 / self.stft.frame_size as f64;
        let start = frame * self.stft.hop_size;
        for i in 0..self.stft.frame_size {
            let slot = (start + i) % self.stft.frame_size;
            self.acc[slot] += self.frame[i] * scale * self.window[i];
            self.nrm[slot] += self.window[i] * self.window[i];
        }
    }

    /// Emit the samples no later frame can reach, and free their slots.
    fn retire(&mut self, frame: usize, length: usize, out: &mut [f32]) {
        let start = frame * self.stft.hop_size;
        for at in start..start + self.stft.hop_size {
            let slot = at % self.stft.frame_size;
            // Undo the analysis padding.
            if let Some(index) = at.checked_sub(self.stft.frame_size)
                && index < length
            {
                // A normalisation near zero means no frame really covered this
                // sample; dividing by it would amplify nothing into noise.
                out[index] = if self.nrm[slot] > 1e-9 {
                    (self.acc[slot] / self.nrm[slot]) as f32
                } else {
                    0.0
                };
            }
            self.acc[slot] = 0.0;
            self.nrm[slot] = 0.0;
        }
    }
}

/// Magnitudes of one frame's bins.
pub fn magnitudes(spectrum: &[Complex64], out: &mut [f64]) {
    for (o, x) in out.iter_mut().zip(spectrum) {
        *o = x.norm();
    }
}

/// Scale a frame's bins by per-bin real gains, in place.
pub fn apply_gains(spectrum: &mut [Complex64], gains: &[f64]) {
    for (x, g) in spectrum.iter_mut().zip(gains) {
        *x *= *g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn signal(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f64;
                (0.4 * (TAU * t / 37.0).sin() + 0.2 * (TAU * t / 5.3).sin()) as f32
            })
            .collect()
    }

    fn worst(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    }

    #[test]
    fn leaving_the_spectrum_alone_returns_the_signal() {
        // The whole contract. Perfect reconstruction, edges included: the
        // padding is what makes the first and last samples come back at full
        // level rather than through the window's taper.
        for stft in [Stft::default(), Stft::new(256, 64), Stft::new(2048, 512)] {
            let input = signal(5_000);
            let out = stft.process(&input, |_, _| {});
            assert_eq!(out.len(), input.len());
            assert!(
                worst(&out, &input) < 1e-6,
                "{stft:?}: worst {}",
                worst(&out, &input)
            );
        }
    }

    #[test]
    fn reconstruction_holds_at_the_very_first_and_last_sample() {
        let input = signal(3_000);
        let out = Stft::default().process(&input, |_, _| {});
        assert!(
            (out[0] - input[0]).abs() < 1e-6,
            "{} vs {}",
            out[0],
            input[0]
        );
        let last = input.len() - 1;
        assert!((out[last] - input[last]).abs() < 1e-6);
    }

    #[test]
    fn the_streaming_and_materialised_paths_agree() {
        // processStft exists to avoid holding the whole spectrogram; it has to
        // give the same answer as holding it, or the memory saving is a change
        // in behaviour wearing a disguise.
        let stft = Stft::default();
        let input = signal(4_000);

        let streamed = stft.process(&input, |spectrum, _| apply_gains(spectrum, &[0.5; 513]));
        let mut analysis = stft.analyze(&input);
        for frame in &mut analysis.frames {
            apply_gains(frame, &[0.5; 513]);
        }
        let materialised = analysis.synthesize();

        assert_eq!(streamed.len(), materialised.len());
        assert!(worst(&streamed, &materialised) < 1e-9);
    }

    #[test]
    fn halving_every_bin_halves_the_signal() {
        let input = signal(3_000);
        let out = Stft::default().process(&input, |spectrum, _| {
            for bin in spectrum.iter_mut() {
                *bin *= 0.5;
            }
        });
        for (o, i) in out.iter().zip(&input) {
            assert!((o - i * 0.5).abs() < 1e-6, "{o} vs {}", i * 0.5);
        }
    }

    #[test]
    fn a_frame_carries_dc_through_nyquist() {
        let stft = Stft::new(256, 64);
        assert_eq!(stft.bins(), 129);
        stft.scan(&signal(1_000), |spectrum, _| {
            assert_eq!(spectrum.len(), 129);
        });
    }

    #[test]
    fn a_tone_lands_in_the_bin_its_frequency_says() {
        let stft = Stft::default();
        let bin = 40;
        let input: Vec<f32> = (0..8_000)
            .map(|i| (TAU * bin as f64 * i as f64 / 1024.0).sin() as f32)
            .collect();

        // Look at a frame in the middle, well clear of the padding.
        let mut seen = None;
        stft.scan(&input, |spectrum, f| {
            if f == 12 {
                seen = Some(
                    spectrum
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
                        .map(|(k, _)| k)
                        .unwrap(),
                );
            }
        });
        assert_eq!(seen, Some(bin));
    }

    #[test]
    fn the_frame_count_matches_what_a_scan_produces() {
        for stft in [Stft::default(), Stft::new(256, 64)] {
            for length in [0usize, 1, 999, 5_000] {
                let counted = stft.frame_count(length);
                let mut seen = 0;
                let visited = stft.scan(&vec![0.0f32; length], |_, _| seen += 1);
                assert_eq!(counted, seen, "{stft:?} at {length}");
                assert_eq!(counted, visited);
            }
        }
    }

    #[test]
    fn silencing_every_frame_gives_silence_and_not_a_division_by_zero() {
        let out = Stft::default().process(&signal(2_000), |spectrum, _| {
            spectrum.fill(Complex64::default());
        });
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn an_empty_signal_processes_to_an_empty_signal() {
        assert!(Stft::default().process(&[], |_, _| {}).is_empty());
        assert_eq!(Stft::default().analyze(&[]).length, 0);
    }

    #[test]
    fn a_signal_shorter_than_a_frame_still_reconstructs() {
        let input = signal(100);
        let out = Stft::default().process(&input, |_, _| {});
        assert_eq!(out.len(), 100);
        assert!(worst(&out, &input) < 1e-6);
    }

    #[test]
    #[should_panic(expected = "frame size must be a power of two")]
    fn a_frame_size_that_is_not_a_power_of_two_is_refused() {
        Stft::new(1000, 250);
    }

    #[test]
    #[should_panic(expected = "whole number of hops")]
    fn a_hop_that_does_not_divide_the_frame_is_refused() {
        Stft::new(1024, 300);
    }

    #[test]
    fn magnitudes_and_gains_do_what_they_say() {
        let mut spectrum = vec![Complex64::new(3.0, 4.0), Complex64::new(0.0, -2.0)];
        let mut mags = vec![0.0; 2];
        magnitudes(&spectrum, &mut mags);
        assert_eq!(mags, vec![5.0, 2.0]);

        apply_gains(&mut spectrum, &[2.0, 0.0]);
        assert_eq!(spectrum[0], Complex64::new(6.0, 8.0));
        assert_eq!(spectrum[1], Complex64::default());
    }
}
