//! The audio device, wrapped around the mixer.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mixer::{Clip, Mixer, Shared};

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("no audio output device")]
    NoDevice,
    #[error("the audio device would not play: {0}")]
    Device(#[from] cpal::Error),
}

/// A set of clips, one playhead, and a device playing them.
pub struct Player {
    shared: Arc<Shared>,
    length: usize,
    sample_rate: u32,
    // Held to keep the stream alive; dropping it closes the device.
    _stream: cpal::Stream,
}

impl Player {
    /// Open the default output device and start feeding it, paused.
    pub fn new(clips: Vec<Clip>) -> Result<Self, PlayerError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(PlayerError::NoDevice)?;
        let config = device.default_output_config()?;
        let channels = config.channels() as usize;
        let sample_rate = config.sample_rate();

        let shared = Arc::new(Shared::default());
        let mut mixer = Mixer::new(clips, shared.clone());
        let length = mixer.length();

        let stream = device.build_output_stream(
            config.into(),
            move |out: &mut [f32], _| mixer.fill(out, channels),
            // A device error is not something the interface can act on, and a
            // panic on the audio thread would take the app with it.
            |error| eprintln!("audio: {error}"),
            None,
        )?;
        stream.play()?;

        Ok(Self {
            shared,
            length,
            sample_rate,
            _stream: stream,
        })
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn length(&self) -> usize {
        self.length
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

    pub fn play(&self) {
        self.shared.finished.store(false, Ordering::Relaxed);
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Whether playback ran off the end since the last [`Player::play`].
    pub fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    pub fn select(&self, clip: usize) {
        self.shared.clip.store(clip, Ordering::Relaxed);
    }

    pub fn selected(&self) -> usize {
        self.shared.clip.load(Ordering::Relaxed)
    }

    pub fn position_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.shared.position.load(Ordering::Relaxed) as f64 / f64::from(self.sample_rate)
    }

    pub fn seek_secs(&self, seconds: f64) {
        let frames = (seconds.max(0.0) * f64::from(self.sample_rate)) as u64;
        self.shared
            .position
            .store(frames.min(self.length as u64), Ordering::Relaxed);
    }

    pub fn set_looping(&self, looping: bool) {
        self.shared.looping.store(looping, Ordering::Relaxed);
    }

    pub fn is_looping(&self) -> bool {
        self.shared.looping.load(Ordering::Relaxed)
    }

    /// The loop region, in seconds. `None` is the whole clip.
    pub fn set_region(&self, region: Option<(f64, f64)>) {
        let (start, end) = match region {
            Some((start, end)) => (
                (start.max(0.0) * f64::from(self.sample_rate)) as u64,
                (end.max(0.0) * f64::from(self.sample_rate)) as u64,
            ),
            None => (0, 0),
        };
        self.shared.region_start.store(start, Ordering::Relaxed);
        self.shared.region_end.store(end, Ordering::Relaxed);
    }

    pub fn region_secs(&self) -> Option<(f64, f64)> {
        let start = self.shared.region_start.load(Ordering::Relaxed);
        let end = self.shared.region_end.load(Ordering::Relaxed);
        if end > start {
            let rate = f64::from(self.sample_rate);
            Some((start as f64 / rate, end as f64 / rate))
        } else {
            None
        }
    }
}
