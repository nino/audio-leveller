//! Band-limited sample-rate conversion, by Kaiser-windowed sinc.
//!
//! The trained stages of the chain run at a fixed rate — 48 kHz for
//! DeepFilterNet-class models — so 44.1 kHz material has to be converted on the
//! way in and back on the way out. Doing that badly undoes exactly the quality
//! the rest of the chain is protecting, so this is a real windowed-sinc
//! resampler and not linear interpolation.
//!
//! The filter is precomputed once into a table indexed by distance from the
//! output position, in sinc zero-crossings, and interpolated linearly between
//! entries. Memory is then fixed whatever the rate ratio: a polyphase bank
//! would want `to_rate / gcd` phases, which explodes for a pathological pair
//! like 44101 → 48000.

use std::f64::consts::PI;

/// Table entries per sinc zero-crossing.
const TABLE_PER_ZERO: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct ResampleOptions {
    /// Sinc zero-crossings kept either side of the output position.
    pub zeros: usize,
    /// Cutoff as a fraction of the lower Nyquist. Below 1 to leave the filter
    /// somewhere to roll off.
    pub rolloff: f64,
    /// Kaiser window shape. Around 12 gives roughly −120 dB of stopband.
    pub beta: f64,
}

impl Default for ResampleOptions {
    fn default() -> Self {
        Self {
            zeros: 32,
            rolloff: 0.95,
            beta: 12.0,
        }
    }
}

/// Modified Bessel function of the first kind, order 0, by its series.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        let ratio = half / f64::from(k);
        term *= ratio * ratio;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

/// The windowed sinc, sampled finely enough to interpolate between entries.
struct FilterTable {
    /// h() every 1/[`TABLE_PER_ZERO`] zero-crossings.
    table: Vec<f64>,
    /// Cutoff in cycles per *input* sample.
    cutoff: f64,
    /// Half the filter's support, in input samples.
    half_width: f64,
}

impl FilterTable {
    fn build(ratio: f64, options: &ResampleOptions) -> Self {
        // Downsampling has to anti-alias to the *output* Nyquist, not the input
        // one — which is what the min() is doing.
        let cutoff = 0.5 * options.rolloff * ratio.min(1.0);
        let half_width = options.zeros as f64 / (2.0 * cutoff);
        let length = options.zeros * TABLE_PER_ZERO + 2;
        let i0_beta = bessel_i0(options.beta);

        let table = (0..length)
            .map(|k| {
                // x is the distance from the centre, in zero-crossings.
                let x = k as f64 / TABLE_PER_ZERO as f64;
                let u = x / options.zeros as f64;
                if u >= 1.0 {
                    return 0.0;
                }
                let window = bessel_i0(options.beta * (1.0 - u * u).sqrt()) / i0_beta;
                2.0 * cutoff * sinc(x) * window
            })
            .collect();

        Self {
            table,
            cutoff,
            half_width,
        }
    }
}

/// How many frames [`resample`] produces from `length` input frames.
pub fn resampled_length(length: usize, from_rate: u32, to_rate: u32) -> usize {
    if from_rate == to_rate {
        return length;
    }
    (length as f64 * f64::from(to_rate) / f64::from(from_rate)).round() as usize
}

/// Resample one channel.
///
/// Samples off either end of the input are taken as zero, but the per-output
/// normalisation divides by the *whole* tap sum, those out-of-range taps
/// included. Dividing by only the in-range sum instead would scale the edge
/// frames up by the reciprocal of whatever fraction of the window survived,
/// turning the boundary into a burst; this way it fades.
fn resample_channel(
    input: &[f32],
    out_length: usize,
    ratio: f64,
    filter: &FilterTable,
) -> Vec<f32> {
    let scale = 2.0 * filter.cutoff * TABLE_PER_ZERO as f64; // input samples → table index
    let max_index = filter.table.len() - 2;

    (0..out_length)
        .map(|m| {
            let centre = m as f64 / ratio;
            let first = (centre - filter.half_width).ceil() as i64;
            let last = (centre + filter.half_width).floor() as i64;

            let mut acc = 0.0;
            let mut weight = 0.0;
            for n in first..=last {
                let index = (centre - n as f64).abs() * scale;
                let i = index as usize;
                if i > max_index {
                    continue;
                }
                let frac = index - i as f64;
                let h = filter.table[i] + frac * (filter.table[i + 1] - filter.table[i]);
                weight += h;
                if let Ok(n) = usize::try_from(n)
                    && n < input.len()
                {
                    acc += f64::from(input[n]) * h;
                }
            }

            if weight != 0.0 {
                (acc / weight) as f32
            } else {
                0.0
            }
        })
        .collect()
}

/// Convert every channel from `from_rate` to `to_rate`, with the default filter.
pub fn resample(channels: &[Vec<f32>], from_rate: u32, to_rate: u32) -> Vec<Vec<f32>> {
    resample_with(channels, from_rate, to_rate, &ResampleOptions::default())
}

/// Convert every channel, with a filter of the caller's choosing.
///
/// Equal rates take a fast path that copies rather than filtering, so a
/// conversion that is not a conversion is exact.
///
/// # Panics
/// If either rate is zero.
pub fn resample_with(
    channels: &[Vec<f32>],
    from_rate: u32,
    to_rate: u32,
    options: &ResampleOptions,
) -> Vec<Vec<f32>> {
    assert!(
        from_rate > 0 && to_rate > 0,
        "sample rates must be positive"
    );
    if from_rate == to_rate {
        return channels.to_vec();
    }

    let ratio = f64::from(to_rate) / f64::from(from_rate);
    let filter = FilterTable::build(ratio, options);

    channels
        .iter()
        .map(|channel| {
            let out_length = resampled_length(channel.len(), from_rate, to_rate);
            resample_channel(channel, out_length, ratio, &filter)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn sine(freq: f64, secs: f64, sample_rate: u32, amplitude: f64) -> Vec<f32> {
        let n = (secs * f64::from(sample_rate)) as usize;
        (0..n)
            .map(|i| (amplitude * (TAU * freq * i as f64 / f64::from(sample_rate)).sin()) as f32)
            .collect()
    }

    /// RMS of the middle of a buffer, away from the edge fades.
    fn core_rms(x: &[f32]) -> f64 {
        let skip = x.len() / 4;
        let core = &x[skip..x.len() - skip];
        (core
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / core.len() as f64)
            .sqrt()
    }

    #[test]
    fn converting_to_the_same_rate_is_exact() {
        let input = vec![sine(1000.0, 0.1, 48_000, 0.5)];
        assert_eq!(resample(&input, 48_000, 48_000), input);
    }

    #[test]
    fn the_output_is_as_long_as_the_rate_ratio_says() {
        assert_eq!(resampled_length(44_100, 44_100, 48_000), 48_000);
        assert_eq!(resampled_length(48_000, 48_000, 44_100), 44_100);
        assert_eq!(resampled_length(0, 44_100, 48_000), 0);

        let input = vec![sine(1000.0, 1.0, 44_100, 0.5)];
        assert_eq!(resample(&input, 44_100, 48_000)[0].len(), 48_000);
    }

    #[test]
    fn a_tone_keeps_its_level_across_a_conversion() {
        let input = vec![sine(1000.0, 1.0, 44_100, 0.5)];
        let out = resample(&input, 44_100, 48_000);
        let ratio = core_rms(&out[0]) / core_rms(&input[0]);
        assert!((ratio - 1.0).abs() < 0.005, "level moved by {ratio}");
    }

    #[test]
    fn a_tone_keeps_its_frequency_across_a_conversion() {
        // Count zero crossings: a 1 kHz tone has 2000 of them per second at any
        // sample rate, and a resampler that got the ratio wrong would not.
        let out = resample(&[sine(1000.0, 1.0, 44_100, 0.5)], 44_100, 48_000);
        let core = &out[0][2_000..46_000];
        let crossings = core
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let expected = 2_000.0 * (44_000.0 / 48_000.0);
        assert!(
            (crossings as f64 - expected).abs() < 4.0,
            "{crossings} crossings, expected about {expected}"
        );
    }

    #[test]
    fn a_round_trip_returns_what_it_was_given() {
        let input = sine(1000.0, 0.5, 48_000, 0.5);
        let there = resample(std::slice::from_ref(&input), 48_000, 44_100);
        let back = resample(&there, 44_100, 48_000);

        assert_eq!(back[0].len(), input.len());
        // Away from the edges, two conversions through a −120 dB filter should
        // cost far less than the 16-bit noise floor.
        let core = 5_000..input.len() - 5_000;
        let worst = core
            .map(|i| (back[0][i] - input[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "worst error {worst}");
    }

    #[test]
    fn downsampling_removes_what_the_new_rate_cannot_hold() {
        // 15 kHz has no home at 24 kHz — above the new Nyquist. An honest
        // resampler filters it out; a naive one folds it back down to 9 kHz.
        let out = resample(&[sine(15_000.0, 0.5, 48_000, 0.5)], 48_000, 24_000);
        assert!(core_rms(&out[0]) < 0.005, "{}", core_rms(&out[0]));
    }

    #[test]
    fn every_channel_is_converted() {
        let input = vec![
            sine(1000.0, 0.2, 48_000, 0.5),
            sine(2000.0, 0.2, 48_000, 0.25),
        ];
        let out = resample(&input, 48_000, 96_000);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 19_200);
        assert!(
            core_rms(&out[1]) < core_rms(&out[0]),
            "channels not mixed up"
        );
    }

    #[test]
    fn silence_stays_silent() {
        let out = resample(&[vec![0.0; 4_800]], 48_000, 44_100);
        assert!(out[0].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn an_empty_channel_converts_to_an_empty_channel() {
        let out = resample(&[Vec::new()], 48_000, 44_100);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_empty());
    }

    #[test]
    #[should_panic(expected = "sample rates must be positive")]
    fn a_zero_rate_is_refused() {
        resample(&[vec![0.0; 8]], 0, 48_000);
    }

    #[test]
    fn the_edges_fade_rather_than_being_extrapolated() {
        // A step into a constant signal rings — that is Gibbs, and any
        // band-limited resampler does it.
        let out = resample(&[vec![1.0f32; 4_800]], 48_000, 44_100);
        let peak = out[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 1.1, "edge peak {peak}");
        assert!((out[0][2_000] - 1.0).abs() < 1e-4, "sag in the middle");
        // Both edges sit below the interior level: half of the first frame's
        // window falls off the front of the buffer, and the sinc's tail off the
        // back. Normalising by only the in-range taps instead would scale those
        // frames back up to 1.0 and past it, inventing signal at the boundary.
        assert!(out[0][0] < 0.95, "first frame {}", out[0][0]);
        assert!(
            *out[0].last().unwrap() < 0.995,
            "last frame {}",
            out[0].last().unwrap()
        );
    }
}
