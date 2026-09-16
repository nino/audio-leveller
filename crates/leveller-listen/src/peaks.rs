//! A multi-resolution min/max cache, for drawing a long file at any zoom.
//!
//! Level 0 holds one min/max pair per [`BASE`] samples, all channels folded
//! together; each level above halves the resolution. Drawing a view picks the
//! finest level whose blocks are still at least twice as fine as a pixel, so
//! every pixel folds a handful of blocks rather than thousands of samples.
//! Zoomed in past [`BASE`] samples per pixel it reads the samples themselves.
//!
//! Two things the browser version had to work around are simply gone here. It
//! kept a 16-bit snapshot of the audio because a decoded `AudioBuffer`'s
//! storage is reclaimable — under memory pressure Chrome purges it page by
//! page, and the waveform visibly "gets quieter" partway through a long
//! session. Rust owns its buffers, so the samples are the samples, kept at full
//! precision. And it had a fallback path for a cache built before that snapshot
//! existed, which a type that cannot be constructed without one does not need.

use leveller_dsp::Signal;

/// Samples per block at the finest level.
pub const BASE: usize = 128;

/// Below this many blocks, halving again buys nothing.
const SMALLEST_LEVEL: usize = 512;

struct Level {
    block: usize,
    min: Vec<f32>,
    max: Vec<f32>,
}

/// The pyramid, and the audio it was built from.
pub struct Peaks {
    samples: Vec<Vec<f32>>,
    length: usize,
    sample_rate: u32,
    levels: Vec<Level>,
}

/// One column of a drawn waveform: the extremes of what it covers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Column {
    pub min: f32,
    pub max: f32,
}

impl Peaks {
    pub fn build(signal: &Signal) -> Self {
        let length = signal.len();
        let blocks = length.div_ceil(BASE).max(1);

        // Folded across channels: the waveform shows the file, not one side of
        // it.
        let mut min = vec![1.0f32; blocks];
        let mut max = vec![-1.0f32; blocks];
        for channel in signal.channels() {
            for (b, (lo, hi)) in min.iter_mut().zip(max.iter_mut()).enumerate() {
                let from = b * BASE;
                let to = (from + BASE).min(length);
                for value in &channel[from..to] {
                    *lo = lo.min(*value);
                    *hi = hi.max(*value);
                }
            }
        }
        // A file shorter than one block leaves the sentinels in place.
        if length == 0 {
            min[0] = 0.0;
            max[0] = 0.0;
        }

        let mut levels = vec![Level {
            block: BASE,
            min,
            max,
        }];
        while levels.last().expect("at least one level").min.len() > SMALLEST_LEVEL {
            let previous = levels.last().expect("at least one level");
            let len = previous.min.len().div_ceil(2);
            let mut min = vec![0.0f32; len];
            let mut max = vec![0.0f32; len];
            for i in 0..len {
                let j = 2 * i;
                let k = (j + 1).min(previous.min.len() - 1);
                min[i] = previous.min[j].min(previous.min[k]);
                max[i] = previous.max[j].max(previous.max[k]);
            }
            levels.push(Level {
                block: previous.block * 2,
                min,
                max,
            });
        }

        Self {
            samples: signal.channels().to_vec(),
            length,
            sample_rate: signal.sample_rate(),
            levels,
        }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.length as f64 / f64::from(self.sample_rate)
        }
    }

    /// One column per pixel, for `width` pixels starting at `start`, at `spp`
    /// samples per pixel.
    ///
    /// Columns past the end of the file come back at zero, and every value lies
    /// in [−1, 1]: the pyramid keeps true extremes internally, and a float file
    /// can exceed full scale, so it is clamped here rather than by every
    /// consumer.
    pub fn columns(&self, start: f64, spp: f64, width: usize) -> Vec<Column> {
        let mut columns = vec![Column::default(); width];
        if width == 0 || self.length == 0 || spp <= 0.0 {
            return columns;
        }

        if spp >= (BASE * 2) as f64 {
            self.pyramid_columns(start, spp, &mut columns);
        } else {
            self.sample_columns(start, spp, &mut columns);
        }
        columns
    }

    fn pyramid_columns(&self, start: f64, spp: f64, columns: &mut [Column]) {
        // The finest level still at least twice as coarse as a pixel.
        let level = self
            .levels
            .iter()
            .rfind(|l| (l.block * 2) as f64 <= spp)
            .unwrap_or(&self.levels[0]);

        for (x, column) in columns.iter_mut().enumerate() {
            let s0 = start + x as f64 * spp;
            if s0 >= self.length as f64 {
                break;
            }
            let b0 = (s0 / level.block as f64).floor().max(0.0) as usize;
            let b1 = (((s0 + spp) / level.block as f64).ceil() as usize).min(level.min.len());

            let mut lo = 1.0f32;
            let mut hi = -1.0f32;
            for b in b0..b1 {
                lo = lo.min(level.min[b]);
                hi = hi.max(level.max[b]);
            }
            // An empty span leaves the sentinels crossed, which must never
            // escape — "unreachable from the current callers" is not a
            // contract.
            *column = if hi < lo {
                Column { min: 0.0, max: 0.0 }
            } else {
                // Clamped at both ends, not only the one each looks like it
                // needs: a file that is entirely above full scale has a
                // *minimum* past 1, and a consumer drawing that would scribble
                // outside its own view.
                Column {
                    min: lo.clamp(-1.0, 1.0),
                    max: hi.clamp(-1.0, 1.0),
                }
            };
        }
    }

    fn sample_columns(&self, start: f64, spp: f64, columns: &mut [Column]) {
        for (x, column) in columns.iter_mut().enumerate() {
            let s0 = (start + x as f64 * spp).floor().max(0.0) as usize;
            if s0 >= self.length {
                break;
            }
            // At least one sample per column, however far in the zoom goes.
            let s1 = ((start + (x + 1) as f64 * spp).floor() as usize)
                .max(s0 + 1)
                .min(self.length);

            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for channel in &self.samples {
                for value in &channel[s0..s1] {
                    lo = lo.min(*value);
                    hi = hi.max(*value);
                }
            }
            *column = if hi < lo {
                Column { min: 0.0, max: 0.0 }
            } else {
                Column {
                    min: lo.clamp(-1.0, 1.0),
                    max: hi.clamp(-1.0, 1.0),
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn signal(n: usize) -> Signal {
        Signal::mono(
            48_000,
            (0..n)
                .map(|i| (0.8 * (TAU * i as f64 / 500.0).sin()) as f32)
                .collect(),
        )
    }

    #[test]
    fn a_column_spans_what_it_covers() {
        let peaks = Peaks::build(&signal(48_000));
        // One column over a whole cycle sees the full swing.
        let columns = peaks.columns(0.0, 500.0, 1);
        assert!(columns[0].max > 0.79, "{:?}", columns[0]);
        assert!(columns[0].min < -0.79, "{:?}", columns[0]);
    }

    #[test]
    fn reading_the_samples_gives_exactly_the_extremes_of_the_window() {
        // The fine path has to be the definition, since it is the one the
        // pyramid is an approximation of.
        let samples = signal(4_000);
        let peaks = Peaks::build(&samples);
        let columns = peaks.columns(0.0, 10.0, 20);

        for (x, column) in columns.iter().enumerate() {
            let window = &samples.channel(0)[x * 10..(x + 1) * 10];
            assert_eq!(column.max, window.iter().copied().fold(f32::MIN, f32::max));
            assert_eq!(column.min, window.iter().copied().fold(f32::MAX, f32::min));
        }
    }

    #[test]
    fn the_pyramid_never_understates_what_the_samples_hold() {
        // Two answers to one question, and the coarse one is only allowed to
        // err outward: a peak may be reported slightly wide, never small.
        let samples = signal(200_000);
        let peaks = Peaks::build(&samples);

        for spp in [256.0f64, 1_024.0, 8_192.0] {
            let width = (200_000.0 / spp) as usize;
            let coarse = peaks.columns(0.0, spp, width);
            for (x, column) in coarse.iter().enumerate() {
                let from = (x as f64 * spp) as usize;
                let to = (((x + 1) as f64 * spp) as usize).min(200_000);
                let window = &samples.channel(0)[from..to];
                let hi = window.iter().copied().fold(f32::MIN, f32::max);
                let lo = window.iter().copied().fold(f32::MAX, f32::min);
                assert!(column.max >= hi - 1e-6, "at {spp}: {column:?} misses {hi}");
                assert!(column.min <= lo + 1e-6, "at {spp}: {column:?} misses {lo}");
            }
        }
    }

    #[test]
    fn zooming_out_never_loses_a_peak() {
        // A single loud sample in a quiet file has to survive every level of
        // the pyramid, or a click becomes invisible at the zoom where you would
        // go looking for it.
        let mut samples = vec![0.01f32; 200_000];
        samples[123_456] = 0.95;
        let peaks = Peaks::build(&Signal::mono(48_000, samples));

        for spp in [1.0f64, 100.0, 1_000.0, 10_000.0] {
            let width = (200_000.0 / spp).ceil() as usize;
            let loudest = peaks
                .columns(0.0, spp, width)
                .into_iter()
                .fold(0.0f32, |m, c| m.max(c.max));
            assert!(loudest > 0.9, "at {spp} samples per pixel: {loudest}");
        }
    }

    #[test]
    fn everything_that_comes_out_is_in_range() {
        // A float file can exceed full scale, and clamping here means no
        // consumer has to.
        let peaks = Peaks::build(&Signal::mono(48_000, vec![1.8, -2.4, 0.0, 1.0]));
        for spp in [0.5, 1.0, 4.0, 400.0] {
            for column in peaks.columns(0.0, spp, 8) {
                assert!((-1.0..=1.0).contains(&column.min), "{column:?}");
                assert!((-1.0..=1.0).contains(&column.max), "{column:?}");
            }
        }
    }

    #[test]
    fn columns_past_the_end_are_flat_rather_than_wrong() {
        let peaks = Peaks::build(&signal(1_000));
        let columns = peaks.columns(0.0, 100.0, 20);
        assert!(columns[0].max > 0.0);
        for column in &columns[10..] {
            assert_eq!(*column, Column::default(), "past the end");
        }
    }

    #[test]
    fn an_empty_file_draws_a_flat_line() {
        let peaks = Peaks::build(&Signal::mono(48_000, Vec::new()));
        assert!(peaks.is_empty());
        assert_eq!(peaks.duration_secs(), 0.0);
        assert!(
            peaks
                .columns(0.0, 100.0, 10)
                .iter()
                .all(|c| *c == Column::default())
        );
    }

    #[test]
    fn a_file_shorter_than_one_block_still_draws() {
        let peaks = Peaks::build(&Signal::mono(48_000, vec![0.5, -0.5]));
        let columns = peaks.columns(0.0, 1.0, 2);
        assert_eq!(columns[0].max, 0.5);
        assert_eq!(columns[1].min, -0.5);
    }

    #[test]
    fn channels_are_folded_rather_than_taking_the_first() {
        // A peak on the right must show, or a fault in one channel is invisible.
        let left = vec![0.1f32; 10_000];
        let mut right = vec![0.1f32; 10_000];
        right[5_000] = 0.9;
        let peaks = Peaks::build(&Signal::new(48_000, vec![left, right]));
        let loudest = peaks
            .columns(0.0, 500.0, 20)
            .into_iter()
            .fold(0.0f32, |m, c| m.max(c.max));
        assert!(loudest > 0.85, "{loudest}");
    }

    #[test]
    fn the_pyramid_stops_when_halving_stops_helping() {
        let peaks = Peaks::build(&signal(4_000_000));
        assert!(peaks.levels.len() > 1, "a long file should have levels");
        assert!(
            peaks.levels.last().unwrap().min.len() <= SMALLEST_LEVEL,
            "it should stop halving"
        );
        // And a short one needs only the base level.
        assert_eq!(Peaks::build(&signal(1_000)).levels.len(), 1);
    }

    #[test]
    fn nonsense_arguments_produce_nothing_rather_than_a_panic() {
        let peaks = Peaks::build(&signal(1_000));
        assert!(peaks.columns(0.0, 100.0, 0).is_empty());
        assert!(
            peaks
                .columns(0.0, 0.0, 4)
                .iter()
                .all(|c| *c == Column::default())
        );
        assert!(peaks.columns(-500.0, 10.0, 4).iter().all(|c| c.min >= -1.0));
        assert!(
            peaks
                .columns(1e12, 10.0, 4)
                .iter()
                .all(|c| *c == Column::default())
        );
    }
}
