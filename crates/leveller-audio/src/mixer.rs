//! What the audio callback does, with no audio device in sight.
//!
//! Separated from the device so it can be tested by calling it: a real-time
//! callback that can only be exercised by playing sound is a callback nobody
//! checks. Everything here runs on the audio thread, so it allocates nothing,
//! locks nothing, and never blocks — which is also why the state it reads is a
//! set of atomics rather than a mutex.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// One clip's audio, ready to play.
pub struct Clip {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
}

impl Clip {
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One sample of one output channel, folding or duplicating as needed.
    ///
    /// A mono clip plays out of both speakers rather than out of the left one,
    /// and a stereo clip on a mono device is summed.
    fn sample(&self, frame: usize, out_channel: usize, out_channels: usize) -> f32 {
        match self.channels.len() {
            0 => 0.0,
            1 => self.channels[0].get(frame).copied().unwrap_or(0.0),
            n if out_channels == 1 => {
                self.channels
                    .iter()
                    .filter_map(|c| c.get(frame))
                    .sum::<f32>()
                    / n as f32
            }
            n => self.channels[out_channel.min(n - 1)]
                .get(frame)
                .copied()
                .unwrap_or(0.0),
        }
    }
}

/// What the main thread tells the audio thread, and what it reads back.
///
/// Every field is an atomic because both threads touch them and the audio one
/// may not wait.
pub struct Shared {
    /// Which clip is playing.
    pub clip: AtomicUsize,
    /// The playhead, in frames.
    pub position: AtomicU64,
    pub playing: AtomicBool,
    pub looping: AtomicBool,
    /// The loop region, in frames. Equal values mean the whole clip.
    pub region_start: AtomicU64,
    pub region_end: AtomicU64,
    /// Set by the callback when it runs off the end without looping, so the
    /// interface can put the button back.
    pub finished: AtomicBool,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            clip: AtomicUsize::new(0),
            position: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(true),
            region_start: AtomicU64::new(0),
            region_end: AtomicU64::new(0),
            finished: AtomicBool::new(false),
        }
    }
}

impl Shared {
    /// The region actually in force: the stored one, or the whole clip.
    fn region(&self, length: u64) -> (u64, u64) {
        let start = self.region_start.load(Ordering::Relaxed).min(length);
        let end = self.region_end.load(Ordering::Relaxed).min(length);
        if end > start {
            (start, end)
        } else {
            (0, length)
        }
    }
}

/// Frames of crossfade when the clip changes under the playhead.
///
/// Four milliseconds at 48 kHz. The point of this player is switching between
/// versions of the same passage mid-word without losing the position, and a
/// hard switch there is a click — which is both unpleasant and, worse, a cue:
/// a listener can hear *that* something changed and start scoring the switch
/// rather than the sound.
const CROSSFADE: usize = 192;

/// Everything the audio thread owns.
pub struct Mixer {
    clips: Vec<Clip>,
    shared: Arc<Shared>,
    /// The clip the last callback was playing, so a change can be faded.
    previous: usize,
    /// How far through a crossfade, in frames.
    fading: usize,
}

impl Mixer {
    pub fn new(clips: Vec<Clip>, shared: Arc<Shared>) -> Self {
        Self {
            clips,
            shared,
            previous: 0,
            fading: 0,
        }
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// Frames in the longest clip, which is what the playhead runs over.
    pub fn length(&self) -> usize {
        self.clips.iter().map(Clip::len).max().unwrap_or(0)
    }

    /// Fill one buffer of interleaved output.
    ///
    /// The whole player is here: one shared playhead across every clip, so
    /// switching mid-word keeps the position exactly, which is the only way to
    /// hear a small difference.
    pub fn fill(&mut self, out: &mut [f32], out_channels: usize) {
        out.fill(0.0);
        if out_channels == 0 || self.clips.is_empty() {
            return;
        }

        let length = self.length() as u64;
        if length == 0 || !self.shared.playing.load(Ordering::Relaxed) {
            return;
        }

        let wanted = self
            .shared
            .clip
            .load(Ordering::Relaxed)
            .min(self.clips.len() - 1);
        if wanted != self.previous {
            self.fading = CROSSFADE;
        }

        let (region_start, region_end) = self.shared.region(length);
        let mut position = self
            .shared
            .position
            .load(Ordering::Relaxed)
            .clamp(region_start, region_end);
        let looping = self.shared.looping.load(Ordering::Relaxed);

        for frame in out.chunks_exact_mut(out_channels) {
            if position >= region_end {
                if looping {
                    position = region_start;
                } else {
                    self.shared.playing.store(false, Ordering::Relaxed);
                    self.shared.finished.store(true, Ordering::Relaxed);
                    break;
                }
            }

            let at = position as usize;
            // A crossfade only while one is running; otherwise this is a plain
            // read from one clip.
            let mix = if self.fading > 0 {
                let t = 1.0 - self.fading as f32 / CROSSFADE as f32;
                self.fading -= 1;
                Some(t)
            } else {
                None
            };

            for (channel, sample) in frame.iter_mut().enumerate() {
                let next = self.clips[wanted].sample(at, channel, out_channels);
                *sample = match mix {
                    None => next,
                    Some(t) => {
                        let old = self.clips[self.previous].sample(at, channel, out_channels);
                        // Equal-power, so the sum holds its level through the
                        // crossfade rather than dipping in the middle.
                        let angle = t * std::f32::consts::FRAC_PI_2;
                        old * angle.cos() + next * angle.sin()
                    }
                };
            }

            position += 1;
        }

        if self.fading == 0 {
            self.previous = wanted;
        }
        self.shared.position.store(position, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(value: f32, len: usize) -> Clip {
        Clip {
            channels: vec![vec![value; len]],
            sample_rate: 48_000,
        }
    }

    /// A clip whose samples are their own index, so where the playhead was is
    /// readable straight off the output.
    fn ramp(len: usize) -> Clip {
        Clip {
            channels: vec![(0..len).map(|i| i as f32).collect()],
            sample_rate: 48_000,
        }
    }

    fn mixer(clips: Vec<Clip>) -> (Mixer, Arc<Shared>) {
        let shared = Arc::new(Shared::default());
        (Mixer::new(clips, shared.clone()), shared)
    }

    #[test]
    fn nothing_comes_out_until_it_is_playing() {
        let (mut mixer, _shared) = mixer(vec![clip(0.5, 100)]);
        let mut out = vec![9.0f32; 16];
        mixer.fill(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.0), "silence, and cleared");
    }

    #[test]
    fn playing_reads_the_clip_and_advances_the_playhead() {
        let (mut mixer, shared) = mixer(vec![ramp(100)]);
        shared.playing.store(true, Ordering::Relaxed);

        let mut out = vec![0.0f32; 8];
        mixer.fill(&mut out, 1);
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(shared.position.load(Ordering::Relaxed), 8);

        mixer.fill(&mut out, 1);
        assert_eq!(out[0], 8.0, "it carried on where it left off");
    }

    #[test]
    fn switching_clips_keeps_the_position() {
        // The whole point of the player: the same moment, a different version.
        let (mut mixer, shared) = mixer(vec![ramp(1_000), ramp(1_000)]);
        shared.playing.store(true, Ordering::Relaxed);

        let mut out = vec![0.0f32; 500];
        mixer.fill(&mut out, 1);
        let position = shared.position.load(Ordering::Relaxed);

        shared.clip.store(1, Ordering::Relaxed);
        mixer.fill(&mut out, 1);
        assert_eq!(
            shared.position.load(Ordering::Relaxed),
            position + 500,
            "the playhead should not have jumped"
        );
    }

    #[test]
    fn a_switch_is_crossfaded_rather_than_cut() {
        // A hard switch is a click, and a click is a cue: a listener hears
        // *that* something changed and starts scoring the switch.
        let (mut mixer, shared) = mixer(vec![clip(1.0, 10_000), clip(-1.0, 10_000)]);
        shared.playing.store(true, Ordering::Relaxed);

        let mut out = vec![0.0f32; 64];
        mixer.fill(&mut out, 1);
        assert!(out.iter().all(|s| (*s - 1.0).abs() < 1e-6));

        shared.clip.store(1, Ordering::Relaxed);
        let mut faded = vec![0.0f32; CROSSFADE + 64];
        mixer.fill(&mut faded, 1);

        // No sample-to-sample jump anywhere near the size of the switch itself.
        let worst = faded
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.05, "a step of {worst} in a 2.0 switch");
        // And it did get all the way to the new clip.
        assert!((faded.last().unwrap() - -1.0).abs() < 1e-6);
    }

    #[test]
    fn a_crossfade_holds_its_level_through_the_middle() {
        // Equal-power, not linear: two uncorrelated clips crossfaded linearly
        // dip by 3 dB halfway, which is audible as a hole.
        let (mut mixer, shared) = mixer(vec![clip(1.0, 10_000), clip(1.0, 10_000)]);
        shared.playing.store(true, Ordering::Relaxed);
        let mut out = vec![0.0f32; 32];
        mixer.fill(&mut out, 1);

        shared.clip.store(1, Ordering::Relaxed);
        let mut faded = vec![0.0f32; CROSSFADE];
        mixer.fill(&mut faded, 1);
        // Identical clips: equal-power crossfading gives up to +3 dB in the
        // middle, and what matters is that it never *dips*.
        let quietest = faded.iter().copied().fold(f32::MAX, f32::min);
        assert!(quietest >= 0.99, "the crossfade dipped to {quietest}");
    }

    #[test]
    fn a_loop_region_repeats_rather_than_running_on() {
        let (mut mixer, shared) = mixer(vec![ramp(1_000)]);
        shared.playing.store(true, Ordering::Relaxed);
        shared.region_start.store(100, Ordering::Relaxed);
        shared.region_end.store(110, Ordering::Relaxed);
        shared.position.store(100, Ordering::Relaxed);

        let mut out = vec![0.0f32; 25];
        mixer.fill(&mut out, 1);
        // Ten frames of region, three times round.
        assert_eq!(
            &out[..10],
            &(100..110).map(|i| i as f32).collect::<Vec<_>>()[..]
        );
        assert_eq!(out[10], 100.0, "it should be back at the start");
        assert_eq!(out[20], 100.0);
        assert!(shared.playing.load(Ordering::Relaxed));
    }

    #[test]
    fn without_looping_it_stops_at_the_end_and_says_so() {
        let (mut mixer, shared) = mixer(vec![ramp(10)]);
        shared.playing.store(true, Ordering::Relaxed);
        shared.looping.store(false, Ordering::Relaxed);

        let mut out = vec![0.0f32; 20];
        mixer.fill(&mut out, 1);
        assert_eq!(out[9], 9.0);
        assert_eq!(out[10], 0.0, "and nothing after the end");
        assert!(!shared.playing.load(Ordering::Relaxed));
        assert!(shared.finished.load(Ordering::Relaxed));
    }

    #[test]
    fn a_mono_clip_plays_out_of_both_speakers() {
        let (mut mixer, shared) = mixer(vec![clip(0.5, 100)]);
        shared.playing.store(true, Ordering::Relaxed);
        let mut out = vec![0.0f32; 8];
        mixer.fill(&mut out, 2);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6), "{out:?}");
    }

    #[test]
    fn a_stereo_clip_on_a_mono_device_is_summed() {
        let stereo = Clip {
            channels: vec![vec![1.0; 100], vec![0.0; 100]],
            sample_rate: 48_000,
        };
        let (mut mixer, shared) = mixer(vec![stereo]);
        shared.playing.store(true, Ordering::Relaxed);
        let mut out = vec![0.0f32; 8];
        mixer.fill(&mut out, 1);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6), "{out:?}");
    }

    #[test]
    fn clips_of_different_lengths_share_one_timeline() {
        // The shorter clip runs out and goes quiet rather than looping early or
        // reading past its end.
        let (mut mixer, shared) = mixer(vec![clip(1.0, 10), clip(1.0, 100)]);
        shared.playing.store(true, Ordering::Relaxed);
        shared.clip.store(0, Ordering::Relaxed);

        let mut out = vec![0.0f32; 20];
        mixer.fill(&mut out, 1);
        assert_eq!(out[5], 1.0);
        assert_eq!(out[15], 0.0, "past the short clip's end");
        assert_eq!(
            shared.position.load(Ordering::Relaxed),
            20,
            "but time went on"
        );
    }

    #[test]
    fn a_clip_index_out_of_range_plays_the_last_one_rather_than_panicking() {
        let (mut mixer, shared) = mixer(vec![clip(0.25, 100)]);
        shared.playing.store(true, Ordering::Relaxed);
        shared.clip.store(99, Ordering::Relaxed);
        let mut out = vec![0.0f32; 8];
        mixer.fill(&mut out, 1);
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn nothing_at_all_is_handled() {
        let mut out = vec![1.0f32; 8];

        let (mut empty, shared) = mixer(Vec::new());
        shared.playing.store(true, Ordering::Relaxed);
        empty.fill(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0));

        out.fill(1.0);
        let (mut silent, shared) = mixer(vec![clip(1.0, 0)]);
        shared.playing.store(true, Ordering::Relaxed);
        silent.fill(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_backwards_region_is_read_as_the_whole_clip() {
        let (mut mixer, shared) = mixer(vec![ramp(100)]);
        shared.playing.store(true, Ordering::Relaxed);
        shared.region_start.store(80, Ordering::Relaxed);
        shared.region_end.store(20, Ordering::Relaxed);

        let mut out = vec![0.0f32; 8];
        mixer.fill(&mut out, 1);
        assert_eq!(out[0], 0.0, "it should start at the beginning");
    }
}
