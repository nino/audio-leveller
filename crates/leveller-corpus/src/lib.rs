//! Deterministic speech-shaped material, and the degradations to inflict on it.
//!
//! This is not speech — it is speech-*shaped*: a glottal source through formant
//! resonances, with a syllable-rate envelope and pauses between talk spurts.
//! That is enough structure for the things the pipeline actually measures —
//! loudness gating, silence detection, noise floor, impulses — while staying
//! reproducible, which real recordings are not.
//!
//! Everything is seeded. No system randomness, so a run today and a run next
//! month give identical numbers, and a metric that moves always means code that
//! moved.
//!
//! Shared by the DSP tests and the evaluation harness, so both are arguing
//! about the same material.

use leveller_dsp::biquad::Biquad;
use leveller_dsp::convolve::convolve;
use leveller_dsp::loudness::Weighted;
use leveller_dsp::{Signal, apply_cascade};

/// Small, fast, seedable PRNG (mulberry32). Uniform in [0, 1).
#[derive(Clone, Debug)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed)
    }

    pub fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }

    /// Uniform in [−1, 1).
    pub fn next_bipolar(&mut self) -> f64 {
        self.next_f64() * 2.0 - 1.0
    }
}

/// `length` samples of uniform noise in [−1, 1).
pub fn noise(length: usize, seed: u32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..length).map(|_| rng.next_bipolar() as f32).collect()
}

/// Integrated loudness of a bare channel, in LUFS.
fn loudness_of(samples: &[f32], sample_rate: u32) -> f64 {
    Weighted::new(std::slice::from_ref(&samples.to_vec()), sample_rate).integrated()
}

/// Scale a channel so its integrated loudness lands on `target_lufs`.
fn normalise_to(samples: &mut [f32], sample_rate: u32, target_lufs: f64) {
    let measured = loudness_of(samples, sample_rate);
    if !measured.is_finite() {
        return;
    }
    let gain = 10f64.powf((target_lufs - measured) / 20.0) as f32;
    for s in samples {
        *s *= gain;
    }
}

/// Vowel inventory: F1/F2/F3 for five vowels, typical adult male values.
///
/// The spurt moves between these, which matters more than it looks. A single
/// fixed vowel leaves a ~13 dB valley between F1 and F2 in the long-term
/// average — deep enough that an injected resonance sitting in it reads as
/// filling a hole rather than as a peak, and any spectral stage evaluated
/// against it learns the wrong lesson. Real speech averages over vowels whose
/// F1 spans 270–730 Hz and F2 840–2290 Hz, which is exactly why measured speech
/// spectra come out smooth.
const VOWELS: [[f64; 3]; 5] = [
    [270.0, 2290.0, 3010.0], // beet
    [530.0, 1840.0, 2480.0], // bet
    [730.0, 1090.0, 2440.0], // father
    [570.0, 840.0, 2410.0],  // boat
    [300.0, 870.0, 2240.0],  // boot
];

/// Peaking filters for one vowel, plus a fixed upper formant.
///
/// Peaking filters rather than two-pole resonators: cascaded resonators each
/// roll off 12 dB per octave above their centre, so four in series fall off a
/// cliff past the top formant and everything above 4 kHz becomes fiction. A
/// peaking filter returns to 0 dB away from its centre, which adds formant
/// contrast without touching the overall tilt.
fn vowel_cascade(vowel: &[f64; 3], sample_rate: f64) -> Vec<Biquad> {
    let gains = [8.0, 6.0, 4.0];
    let qs = [1.2, 1.2, 1.5];
    let mut bands: Vec<Biquad> = vowel
        .iter()
        .enumerate()
        .map(|(i, freq)| Biquad::peaking(sample_rate, *freq, gains[i], qs[i]))
        .collect();
    bands.push(Biquad::peaking(sample_rate, 3_700.0, 3.0, 1.5));
    bands
}

/// One talk spurt at unit scale, by source-filter synthesis.
///
/// An additive harmonic stack was the obvious approach and the wrong one: any
/// finite number of harmonics puts a cliff in the spectrum — sixteen harmonics
/// of a 110 Hz voice ends at 1.8 kHz — and everything above it is silence
/// dressed as signal, useless for judging anything spectral. A glottal pulse
/// train through formant resonators produces energy all the way to Nyquist,
/// from the same mechanism real speech uses, at a fraction of the cost.
fn talk_spurt(seconds: f64, sample_rate: u32, f0: f64, seed: u32) -> Vec<f32> {
    let rate = f64::from(sample_rate);
    let n = (seconds * rate).round() as usize;
    let mut rng = Rng::new(seed);
    let syllable_rate = 3.8 + rng.next_f64() * 1.2;
    let syllable_phase = rng.next_f64() * std::f64::consts::TAU;
    let fricative = noise(n, seed + 1);

    // Prosody. Without it the harmonics sit at exactly the same frequencies for
    // the whole file and the long-term average resolves a deep comb rather than
    // a spectral envelope — an artefact no real voice produces, and one that
    // would send any spectral stage chasing the pitch rather than the room.
    // Real speech moves: pitch declines across an utterance, rises and falls
    // with intonation, and jitters cycle to cycle.
    let declination_depth = 0.12; // ~+12% at the start to −12% at the end
    let mut jitter = 0.0f64;

    // Glottal source: a train of Rosenberg pulses. The shape is smooth, so its
    // spectrum falls at roughly −12 dB/octave the way real glottal flow does,
    // without the aliasing a naive sawtooth would bring.
    let open_quotient = 0.4;
    let close_quotient = 0.16;
    let mut cycle = 0.0f64; // position within the current pitch period, in [0, 1)

    let mut out = vec![0.0f32; n];
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f64 / rate;
        let declination =
            1.0 + declination_depth * (1.0 - 2.0 * i as f64 / (n.saturating_sub(1)).max(1) as f64);
        let intonation = 1.0 + 0.06 * (std::f64::consts::TAU * 0.7 * t + syllable_phase).sin();
        // A slow bounded random walk, for cycle-to-cycle variation.
        jitter = (jitter + (rng.next_f64() - 0.5) * 0.0008).clamp(-0.03, 0.03);

        let pitch = f0 * declination * intonation * (1.0 + jitter);
        cycle += pitch / rate;
        if cycle >= 1.0 {
            cycle -= 1.0;
        }

        let pulse = if cycle < open_quotient {
            0.5 * (1.0 - (std::f64::consts::PI * cycle / open_quotient).cos())
        } else if cycle < open_quotient + close_quotient {
            (std::f64::consts::PI * (cycle - open_quotient) / (2.0 * close_quotient)).cos()
        } else {
            0.0
        };

        // Aspiration rides with the voicing, as in real speech. Kept low: the
        // radiation difference below applies +6 dB/octave to it as well, and too
        // much turns the top of the spectrum into rising noise rather than the
        // falling tilt real speech has.
        *sample = (pulse - 0.5 + f64::from(fricative[i]) * 0.004) as f32;
    }

    // Lip radiation is a first difference, +6 dB/octave, which turns the
    // source's −12 into the ~−6 dB/octave tilt measured speech shows.
    let mut previous = 0.0f32;
    for sample in &mut out {
        let current = *sample;
        *sample = current - 0.97 * previous;
        previous = current;
    }

    // Vocal-tract colouring, moving between vowels. Each vowel filters the
    // whole source once and the spurt is assembled from chunks of the results
    // with short crossfades — cheaper than a time-varying filter, and free of
    // the transients switching coefficients mid-stream would introduce.
    let tracks: Vec<Vec<f32>> = VOWELS
        .iter()
        .map(|vowel| apply_cascade(&out, &vowel_cascade(vowel, rate)))
        .collect();
    let chunk = ((0.18 * rate).round() as usize).max(1);
    let fade = ((0.02 * rate).round() as usize).max(1);

    let mut previous_track =
        &tracks[(rng.next_f64() * VOWELS.len() as f64) as usize % VOWELS.len()];
    let mut start = 0usize;
    while start < n {
        let end = (start + chunk).min(n);
        let track = &tracks[(rng.next_f64() * VOWELS.len() as f64) as usize % VOWELS.len()];
        for i in start..end {
            let into = i - start;
            if into < fade && start > 0 {
                // Equal-power crossfade from the previous vowel.
                let t = into as f64 / fade as f64 * std::f64::consts::FRAC_PI_2;
                out[i] =
                    (f64::from(previous_track[i]) * t.cos() + f64::from(track[i]) * t.sin()) as f32;
            } else {
                out[i] = track[i];
            }
        }
        previous_track = track;
        start += chunk;
    }

    // Syllable-rate amplitude envelope and edge fades, applied last so the
    // filters saw a continuously voiced excitation.
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f64 / rate;
        let syllable = 0.25
            + 0.75
                * (0.5 - 0.5 * (std::f64::consts::TAU * syllable_rate * t + syllable_phase).cos())
                    .powf(0.6);
        // A gentle fade at the spurt edges, so onsets are not clicks themselves.
        let edge = (i.min(n - 1 - i) as f64 / (0.02 * rate)).min(1.0);
        *sample = (f64::from(*sample) * syllable * edge) as f32;
    }

    // Bring the spurt to a sane working level before anything measures it. The
    // cascade's absolute output depends on the formant bandwidths and the
    // radiation difference, and lands far below the −70 LUFS absolute gate —
    // which would make the loudness measurement return −∞ and the caller's
    // normalisation silently do nothing.
    let energy: f64 = out.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms = (energy / n.max(1) as f64).sqrt();
    if rms > 0.0 {
        let scale = (0.1 / rms) as f32;
        for sample in &mut out {
            *sample *= scale;
        }
    }

    out
}

/// One talk spurt of the finished recording, and the level it was built at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeechSegment {
    /// Sample range of the spurt, not counting the pauses around it.
    pub start: usize,
    pub end: usize,
    pub level_lufs: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Spurt {
    pub seconds: f64,
    pub level_lufs: f64,
    /// Fundamental. `None` walks up from 105 Hz, 25 Hz per spurt.
    pub f0: Option<f64>,
}

impl Spurt {
    pub fn new(seconds: f64, level_lufs: f64) -> Self {
        Self {
            seconds,
            level_lufs,
            f0: None,
        }
    }

    pub fn at_pitch(mut self, f0: f64) -> Self {
        self.f0 = Some(f0);
        self
    }
}

#[derive(Clone, Debug)]
pub struct SpeechOptions {
    pub sample_rate: u32,
    pub spurts: Vec<Spurt>,
    /// Pause between spurts, and before the first and after the last.
    pub pause_sec: f64,
    /// Steady noise floor across the whole file, in dBFS RMS.
    pub floor_dbfs: f64,
    pub seed: u32,
    /// Stereo duplicates the mono programme under an independent floor.
    pub channels: usize,
}

impl Default for SpeechOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            spurts: vec![Spurt::new(3.0, -23.0)],
            pause_sec: 0.5,
            floor_dbfs: -62.0,
            seed: 4242,
            channels: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Speech {
    pub signal: Signal,
    pub segments: Vec<SpeechSegment>,
}

/// Build a recording: pause, spurt, pause, spurt, …
///
/// Each spurt is normalised to its requested loudness *before* the noise floor
/// goes on, so the segment levels are exact and known — which is what makes it
/// possible to assert that the leveller hit them.
pub fn synthetic_speech(options: &SpeechOptions) -> Speech {
    let sample_rate = options.sample_rate;
    let pause = (options.pause_sec * f64::from(sample_rate)).round() as usize;

    let spurts: Vec<Vec<f32>> = options
        .spurts
        .iter()
        .enumerate()
        .map(|(i, spurt)| {
            let f0 = spurt.f0.unwrap_or(105.0 + i as f64 * 25.0);
            let mut samples = talk_spurt(
                spurt.seconds,
                sample_rate,
                f0,
                options.seed.wrapping_add(i as u32 * 977),
            );
            normalise_to(&mut samples, sample_rate, spurt.level_lufs);
            samples
        })
        .collect();

    let total = pause * (spurts.len() + 1) + spurts.iter().map(Vec::len).sum::<usize>();
    let mut programme = vec![0.0f32; total];
    let mut segments = Vec::with_capacity(spurts.len());
    let mut offset = pause;
    for (i, spurt) in spurts.iter().enumerate() {
        programme[offset..offset + spurt.len()].copy_from_slice(spurt);
        segments.push(SpeechSegment {
            start: offset,
            end: offset + spurt.len(),
            level_lufs: options.spurts[i].level_lufs,
        });
        offset += spurt.len() + pause;
    }

    // A steady floor everywhere, under the speech included.
    let floor_amplitude = (10f64.powf(options.floor_dbfs / 20.0) * std::f64::consts::SQRT_2) as f32;
    let channels = (0..options.channels)
        .map(|c| {
            let floor = noise(total, options.seed.wrapping_add(5_000 + c as u32 * 31));
            programme
                .iter()
                .zip(&floor)
                .map(|(p, f)| p + f * floor_amplitude)
                .collect()
        })
        .collect();

    Speech {
        signal: Signal::new(sample_rate, channels),
        segments,
    }
}

/// Add broadband noise at a requested signal-to-noise ratio, measured as the
/// integrated loudness of the programme against that of the noise.
pub fn add_noise(signal: &Signal, snr_db: f64, seed: u32) -> Signal {
    let programme = Weighted::new(signal.channels(), signal.sample_rate()).integrated();
    if !programme.is_finite() {
        return signal.clone();
    }

    let channels = signal
        .channels()
        .iter()
        .enumerate()
        .map(|(c, channel)| {
            let noise = noise(signal.len(), seed.wrapping_add(c as u32 * 101));
            // Scale the noise so that on its own it measures
            // (programme − snr) LUFS.
            let noise_lufs = loudness_of(&noise, signal.sample_rate());
            let gain = 10f64.powf((programme - snr_db - noise_lufs) / 20.0) as f32;
            channel
                .iter()
                .zip(&noise)
                .map(|(s, n)| s + n * gain)
                .collect()
        })
        .collect();

    Signal::new(signal.sample_rate(), channels)
}

#[derive(Clone, Copy, Debug)]
pub struct ClickOptions {
    pub count: usize,
    /// Peak click amplitude relative to the signal's peak; 1 is as loud as it.
    pub relative_amplitude: f64,
    /// Click width. Real clicks are a handful of samples.
    pub width_samples: usize,
    /// Keep neighbouring clicks at least this far apart.
    ///
    /// The de-clicker treats two comparable impulses one pitch period apart as
    /// voicing, by design; dense injection would manufacture exactly that and
    /// then score it as a miss.
    pub min_gap_sec: f64,
    pub seed: u32,
}

impl Default for ClickOptions {
    fn default() -> Self {
        Self {
            count: 30,
            relative_amplitude: 2.0,
            width_samples: 3,
            min_gap_sec: 0.05,
            seed: 99,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Clicked {
    pub signal: Signal,
    /// Where each click went, for scoring what survived.
    pub positions: Vec<usize>,
}

/// Scatter impulsive clicks through a signal — the digital-dropout kind, a
/// couple of samples wide with a sharp bipolar shape.
pub fn add_clicks(signal: &Signal, options: &ClickOptions) -> Clicked {
    let mut out = signal.clone();
    let amplitude = (f64::from(signal.peak()) * options.relative_amplitude) as f32;

    let mut rng = Rng::new(options.seed);
    let margin = (options.width_samples * 4).max(64);
    let span = signal.len().saturating_sub(2 * margin);
    // Jitter is bounded so neighbouring clicks stay `min_gap_sec` apart.
    let jitter_span = (1.0 / options.count as f64
        - options.min_gap_sec * f64::from(signal.sample_rate()) / span.max(1) as f64)
        .max(0.0);

    let mut positions = Vec::with_capacity(options.count);
    for k in 0..options.count {
        // Evenly spread with jitter, so clicks land in speech and in pauses
        // alike.
        let slot = (k as f64 + 0.5) / options.count as f64;
        let jitter = (rng.next_f64() - 0.5) * jitter_span;
        let at = ((slot + jitter) * span as f64).round() as usize + margin;
        positions.push(at);

        for channel in out.channels_mut() {
            for w in 0..options.width_samples {
                // Alternating sign: a sharp bipolar spike, broadband by
                // construction.
                let sign = if w % 2 == 0 { 1.0 } else { -1.0 };
                let shape = sign * (1.0 - w as f32 / options.width_samples as f32);
                if let Some(sample) = channel.get_mut(at + w) {
                    *sample += amplitude * shape;
                }
            }
        }
    }

    positions.sort_unstable();
    Clicked {
        signal: out,
        positions,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RirOptions {
    /// Time for the tail to fall 60 dB, in seconds.
    pub rt60_sec: f64,
    /// How far the reverberant energy sits below the direct sound, in dB.
    pub direct_to_reverb_db: f64,
    /// Gap between the direct sound and the first reflection.
    pub pre_delay_sec: f64,
    pub seed: u32,
}

impl Default for RirOptions {
    fn default() -> Self {
        Self {
            rt60_sec: 0.6,
            direct_to_reverb_db: 6.0,
            pre_delay_sec: 0.012,
            seed: 31_337,
        }
    }
}

/// A synthetic room impulse response: direct sound, then an exponentially
/// decaying noise tail after a pre-delay.
///
/// Real rooms have discrete early reflections before the tail goes diffuse, and
/// a decay that varies with frequency — high frequencies die first. This models
/// the decay envelope and the direct-to-reverberant ratio, which are what a
/// dereverberator has to work against, and is honest about being a model rather
/// than a measurement.
pub fn synthetic_rir(sample_rate: u32, options: &RirOptions) -> Vec<f32> {
    let rate = f64::from(sample_rate);
    let length = ((options.rt60_sec * 1.5 * rate).round() as usize).max(2);
    let pre_delay = (options.pre_delay_sec * rate).round() as usize;
    let mut rng = Rng::new(options.seed);

    let mut rir = vec![0.0f32; length];
    rir[0] = 1.0; // direct sound

    // Diffuse tail: noise under an exponential envelope reaching −60 dB at rt60.
    let decay = 1000f64.ln() / (options.rt60_sec * rate);
    let mut tail_energy = 0.0f64;
    for (i, sample) in rir[pre_delay..].iter_mut().enumerate() {
        let envelope = (-decay * i as f64).exp();
        let value = rng.next_bipolar() * envelope;
        *sample = value as f32;
        tail_energy += value * value;
    }

    // Scale the tail so the direct-to-reverberant ratio comes out as asked.
    if tail_energy > 0.0 {
        let wanted = 10f64.powf(-options.direct_to_reverb_db / 10.0);
        let scale = (wanted / tail_energy).sqrt() as f32;
        for sample in &mut rir[pre_delay..] {
            *sample *= scale;
        }
    }

    rir
}

/// Put a signal in a room.
///
/// Convolution adds energy, so the result is renormalised to the dry loudness:
/// what the metrics then see is reverberation, not a level change.
pub fn add_reverb(signal: &Signal, options: &RirOptions) -> Signal {
    let rir = synthetic_rir(signal.sample_rate(), options);
    let mut channels: Vec<Vec<f32>> = signal
        .channels()
        .iter()
        .map(|c| convolve(c, &rir))
        .collect();

    let dry = Weighted::new(signal.channels(), signal.sample_rate()).integrated();
    let wet = Weighted::new(&channels, signal.sample_rate()).integrated();
    if dry.is_finite() && wet.is_finite() {
        let gain = 10f64.powf((dry - wet) / 20.0) as f32;
        for channel in &mut channels {
            for sample in channel {
                *sample *= gain;
            }
        }
    }

    Signal::new(signal.sample_rate(), channels)
}

#[cfg(test)]
// `&[a..b]` here is a one-element slice of ranges, which is what the LTAS takes;
// clippy reads it as a mistyped `vec![a; b]`.
#[expect(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_reproducible() {
        let options = SpeechOptions::default();
        assert_eq!(
            synthetic_speech(&options).signal,
            synthetic_speech(&options).signal
        );
    }

    #[test]
    fn a_different_seed_gives_different_material() {
        let a = synthetic_speech(&SpeechOptions::default());
        let b = synthetic_speech(&SpeechOptions {
            seed: 7,
            ..SpeechOptions::default()
        });
        assert_ne!(a.signal, b.signal);
    }

    #[test]
    fn each_spurt_lands_on_the_level_it_was_asked_for() {
        let speech = synthetic_speech(&SpeechOptions {
            spurts: vec![Spurt::new(3.0, -23.0), Spurt::new(3.0, -31.0)],
            ..SpeechOptions::default()
        });
        for segment in &speech.segments {
            let channel = &speech.signal.channel(0)[segment.start..segment.end];
            let measured =
                Weighted::new(std::slice::from_ref(&channel.to_vec()), 48_000).integrated();
            assert!(
                (measured - segment.level_lufs).abs() < 0.6,
                "wanted {} LUFS, measured {measured}",
                segment.level_lufs
            );
        }
    }

    #[test]
    fn the_pauses_are_where_the_segments_say_they_are_not() {
        let speech = synthetic_speech(&SpeechOptions {
            spurts: vec![Spurt::new(2.0, -23.0), Spurt::new(2.0, -23.0)],
            pause_sec: 1.0,
            ..SpeechOptions::default()
        });
        assert_eq!(speech.segments.len(), 2);
        assert_eq!(speech.segments[0].start, 48_000);
        assert_eq!(speech.segments[1].start, speech.segments[0].end + 48_000);
        assert_eq!(speech.signal.len(), speech.segments[1].end + 48_000);
    }

    #[test]
    fn the_spectrum_reaches_the_top_of_the_band() {
        // The reason for source-filter synthesis over an additive stack: a
        // finite harmonic sum ends in a cliff, and everything above it is
        // silence dressed as signal.
        use leveller_dsp::ltas;
        let speech = synthetic_speech(&SpeechOptions::default());
        let segment = speech.segments[0];
        let ltas = ltas::compute(
            speech.signal.channel(0),
            48_000,
            &[segment.start..segment.end],
            &ltas::LtasOptions::default(),
        )
        .expect("enough frames");

        let level_at = |freq: f64| -> f64 {
            let i = ltas
                .freqs
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1 - freq).abs().total_cmp(&(b.1 - freq).abs()))
                .map(|(i, _)| i)
                .unwrap();
            ltas.db[i]
        };
        // 8 kHz should be down on 500 Hz, but present — not a hole.
        assert!(
            level_at(8_000.0) > level_at(500.0) - 45.0,
            "cliff at the top"
        );
        assert!(level_at(8_000.0) < level_at(500.0), "the tilt should fall");
    }

    #[test]
    fn adding_noise_hits_the_ratio_it_was_asked_for() {
        let speech = synthetic_speech(&SpeechOptions {
            floor_dbfs: -120.0,
            ..SpeechOptions::default()
        });
        let clean = Weighted::new(speech.signal.channels(), 48_000).integrated();
        let noisy = add_noise(&speech.signal, 20.0, 5);

        // Measure the noise alone, as the difference.
        let difference: Vec<f32> = noisy
            .channel(0)
            .iter()
            .zip(speech.signal.channel(0))
            .map(|(a, b)| a - b)
            .collect();
        let noise_lufs = Weighted::new(std::slice::from_ref(&difference), 48_000).integrated();
        assert!(
            (clean - noise_lufs - 20.0).abs() < 0.5,
            "{clean} vs {noise_lufs}"
        );
    }

    #[test]
    fn clicks_land_where_they_are_reported() {
        let speech = synthetic_speech(&SpeechOptions::default());
        let peak = speech.signal.peak();
        let clicked = add_clicks(&speech.signal, &ClickOptions::default());

        assert_eq!(clicked.positions.len(), 30);
        assert!(clicked.positions.windows(2).all(|w| w[0] < w[1]), "sorted");
        for at in &clicked.positions {
            let before = speech.signal.channel(0)[*at];
            let after = clicked.signal.channel(0)[*at];
            assert!((after - before).abs() > peak, "no click at {at}");
        }
    }

    #[test]
    fn clicks_are_kept_apart_so_they_do_not_look_like_voicing() {
        let speech = synthetic_speech(&SpeechOptions::default());
        let clicked = add_clicks(&speech.signal, &ClickOptions::default());
        let gap = (0.05 * 48_000.0) as usize;
        for pair in clicked.positions.windows(2) {
            assert!(pair[1] - pair[0] >= gap, "{} then {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn a_room_decays_by_sixty_db_over_its_rt60() {
        let options = RirOptions::default();
        let rir = synthetic_rir(48_000, &options);
        let at = |sec: f64| -> f64 {
            let i = (sec * 48_000.0) as usize;
            let window = &rir[i..(i + 480).min(rir.len())];
            let energy: f64 = window.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            10.0 * (energy / window.len() as f64 + 1e-30).log10()
        };
        let early = at(0.02);
        let late = at(0.62);
        assert!((early - late - 60.0).abs() < 6.0, "{early} then {late}");
    }

    #[test]
    fn reverberation_does_not_change_the_level() {
        let speech = synthetic_speech(&SpeechOptions::default());
        let wet = add_reverb(&speech.signal, &RirOptions::default());
        let dry_lufs = Weighted::new(speech.signal.channels(), 48_000).integrated();
        let wet_lufs = Weighted::new(wet.channels(), 48_000).integrated();
        assert!(
            (dry_lufs - wet_lufs).abs() < 0.1,
            "{dry_lufs} vs {wet_lufs}"
        );
    }

    #[test]
    fn reverberation_makes_the_recording_ring_for_longer() {
        use leveller_dsp::reverb_decay;
        let speech = synthetic_speech(&SpeechOptions {
            spurts: vec![Spurt::new(4.0, -23.0)],
            ..SpeechOptions::default()
        });
        let wet = add_reverb(&speech.signal, &RirOptions::default());
        let dry_ms = reverb_decay(&speech.signal).expect("dry");
        let wet_ms = reverb_decay(&wet).expect("wet");
        assert!(wet_ms > dry_ms, "dry {dry_ms} ms, wet {wet_ms} ms");
    }
}
