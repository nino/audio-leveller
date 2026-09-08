//! Blind reverberation-time measurement.
//!
//! Lives with the DSP rather than with the evaluation metrics because the
//! dereverb stage needs it at runtime, to decide whether to engage at all — and
//! a stage reaching into the test fixtures for a measurement would be exactly
//! backwards.

use crate::Signal;

/// Blind estimate of how long the recording rings, in milliseconds.
///
/// No reference and no impulse response: find the moments where speech stops,
/// and measure how fast the energy falls afterwards. Dry speech decays at the
/// rate the talker's own articulation sets — a syllable ends in tens of
/// milliseconds. A room adds its own, much slower decay on top, so this number
/// rises with reverberation and falls when a dereverberator works.
///
/// Reported as the time to fall 20 dB from each offset, taken as a median
/// across offsets: the same idea as a room's T20, without needing to fire a
/// starter pistol in it.
///
/// Returns `None` when there is too little material, or nothing in it that
/// looks like a decay.
pub fn reverb_decay_ms(signal: &Signal, frame_ms: f64) -> Option<f64> {
    let frame = ((frame_ms / 1000.0 * f64::from(signal.sample_rate())).round() as usize).max(1);
    let frames = signal.len() / frame;
    if frames < 10 {
        return None;
    }

    // Broadband energy envelope, in dB.
    let channels = signal.channel_count();
    let envelope: Vec<f64> = (0..frames)
        .map(|f| {
            let acc: f64 = signal
                .channels()
                .iter()
                .flat_map(|c| &c[f * frame..(f + 1) * frame])
                .map(|s| f64::from(*s) * f64::from(*s))
                .sum();
            10.0 * (acc / (frame * channels) as f64 + 1e-30).log10()
        })
        .collect();

    // Only consider offsets from frames loud enough to be speech, so the
    // measurement is not dominated by a wandering noise floor.
    let peak = envelope.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let active = peak - 25.0;

    let mut decays: Vec<f64> = Vec::new();
    for f in 1..frames - 1 {
        if envelope[f] < active {
            continue;
        }
        // An offset is a local maximum followed by a sustained fall.
        if envelope[f] < envelope[f - 1] || envelope[f] < envelope[f + 1] {
            continue;
        }

        let start = envelope[f];
        let after = &envelope[f + 1..frames.min(f + 200)];
        for (g, level) in after.iter().enumerate() {
            // A rise means new speech began; this offset tells us nothing.
            if *level > start {
                break;
            }
            if *level <= start - 20.0 {
                decays.push((g + 1) as f64 * frame_ms);
                break;
            }
        }
    }

    median(&mut decays)
}

/// [`reverb_decay_ms`] at the 5 ms frame the dereverb stage uses.
pub fn reverb_decay(signal: &Signal) -> Option<f64> {
    reverb_decay_ms(signal, 5.0)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convolve::{convolve, synthetic_impulse};
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    /// Syllable-shaped bursts: a fast attack and a decay of the talker's own,
    /// with gaps between them. Flat-topped bursts would not do — the offset
    /// detector looks for a local maximum, and every frame of a flat top is
    /// one, so the decay it then measured would be the length of the burst.
    fn bursts(count: usize, burst_ms: f64, gap_ms: f64) -> Signal {
        let burst = (burst_ms / 1000.0 * f64::from(SR)) as usize;
        let gap = (gap_ms / 1000.0 * f64::from(SR)) as usize;
        // 15 ms of decay: ~35 ms to fall 20 dB, which is what articulation does.
        let tau = 0.015 * f64::from(SR);
        let attack = 0.002 * f64::from(SR);

        let mut samples = Vec::new();
        for _ in 0..count {
            for i in 0..burst {
                let t = i as f64;
                let envelope = (1.0 - (-t / attack).exp()) * (-t / tau).exp();
                samples.push((0.5 * envelope * (TAU * 400.0 * t / f64::from(SR)).sin()) as f32);
            }
            samples.extend(std::iter::repeat_n(0.0f32, gap));
        }
        Signal::mono(SR, samples)
    }

    #[test]
    fn a_room_makes_the_measurement_longer() {
        // The property the dereverb stage relies on. Absolute values from a
        // blind estimate are not worth asserting; the ordering is.
        let dry = bursts(20, 300.0, 300.0);
        let wet = Signal::mono(SR, convolve(dry.channel(0), &synthetic_impulse(SR, 0.5, 3)));

        let dry_ms = reverb_decay(&dry).expect("dry decay");
        let wet_ms = reverb_decay(&wet).expect("wet decay");
        assert!(wet_ms > dry_ms * 1.5, "dry {dry_ms} ms, wet {wet_ms} ms");
    }

    #[test]
    fn a_longer_room_measures_longer_than_a_shorter_one() {
        let dry = bursts(20, 300.0, 300.0);
        let short = Signal::mono(SR, convolve(dry.channel(0), &synthetic_impulse(SR, 0.2, 3)));
        let long = Signal::mono(SR, convolve(dry.channel(0), &synthetic_impulse(SR, 0.8, 3)));

        let short_ms = reverb_decay(&short).expect("short decay");
        let long_ms = reverb_decay(&long).expect("long decay");
        assert!(long_ms > short_ms, "short {short_ms} ms, long {long_ms} ms");
    }

    #[test]
    fn too_short_a_signal_measures_nothing() {
        assert!(reverb_decay(&Signal::mono(SR, vec![0.0; 100])).is_none());
        assert!(reverb_decay(&Signal::mono(SR, Vec::new())).is_none());
    }

    #[test]
    fn a_signal_with_no_decay_in_it_measures_nothing() {
        // Steady tone: no offsets, so no decay to measure. The answer is "I
        // cannot tell", not a number.
        let steady: Vec<f32> = (0..SR as usize * 2)
            .map(|i| (0.5 * (TAU * 400.0 * i as f64 / f64::from(SR)).sin()) as f32)
            .collect();
        assert!(reverb_decay(&Signal::mono(SR, steady)).is_none());
    }

    #[test]
    fn digital_silence_measures_nothing() {
        assert!(reverb_decay(&Signal::silence(SR, 1, SR as usize)).is_none());
    }

    #[test]
    fn a_finer_frame_gives_a_comparable_answer() {
        let dry = bursts(20, 300.0, 300.0);
        let wet = Signal::mono(SR, convolve(dry.channel(0), &synthetic_impulse(SR, 0.5, 3)));
        let coarse = reverb_decay_ms(&wet, 10.0).expect("coarse");
        let fine = reverb_decay_ms(&wet, 2.0).expect("fine");
        // Not equal — the envelope is smoothed differently — but the same
        // order, or the measurement is telling you about the frame size rather
        // than about the room.
        assert!(
            fine > coarse * 0.4 && fine < coarse * 2.5,
            "{coarse} vs {fine}"
        );
    }

    #[test]
    fn stereo_is_measured_across_both_channels() {
        let dry = bursts(20, 300.0, 300.0);
        let wet = convolve(dry.channel(0), &synthetic_impulse(SR, 0.5, 3));
        let mono = reverb_decay(&Signal::mono(SR, wet.clone())).expect("mono");
        let stereo = reverb_decay(&Signal::new(SR, vec![wet.clone(), wet])).expect("stereo");
        assert_eq!(mono, stereo, "duplicating a channel should change nothing");
    }
}
