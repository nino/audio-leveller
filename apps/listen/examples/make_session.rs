//! Build a listening session out of synthetic audio, for the screenshots and
//! for trying the app without a real recording to hand.
//!
//! Takes the directory to write into as its one argument, and insists on it:
//! the obvious default would be `./listening`, which is where the real sessions
//! live, and a synthetic `chili-15` sitting next to a real `chili` is a trap
//! rather than a convenience.
//!
//! What it writes is the layout the real sessions use: `sessions/<name>/
//! session.json`, its `key.json`, one WAV per clip, and a file under
//! `annotate/` with something worth marking in it.
//!
//! The clips are the same passage put through different processing, which is
//! what a real session is — otherwise the test is "which recording do you
//! prefer" rather than "which processing".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use leveller_corpus::{ClickOptions, RirOptions, SpeechOptions, Spurt, add_clicks, add_reverb};
use leveller_dsp::Signal;
use leveller_listen::{
    Clip, MatchBy, Session, SessionKey, Trial, Window, default_clip_questions,
    default_trial_questions, store,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!(
            "Usage: cargo run -p listen-app --example make_session -- <directory>\n\n\
             Writes a synthetic session there. Give it a scratch directory rather \n\
             than your real listening root: the material is fake, and mixing it in \n\
             with recordings you actually care about is how a listening test ends \n\
             up scoring a sine wave."
        );
        std::process::exit(2);
    };
    let name = "chili-15";
    let dir = root.join("sessions").join(name);
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(root.join("annotate"))?;

    // One passage, three treatments. The blinding is the point: the labels are
    // A, B and C in the session and the variants are only named in the key.
    let mut trials = Vec::new();
    let mut clips_by_trial: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut windows = BTreeMap::new();

    for (index, (id, title, seed)) in [
        ("t01", "The loud passage, two spurts and a pause", 11u32),
        ("t02", "The quiet one, near the noise floor", 23),
        ("t03", "Room tone under a long sentence", 37),
    ]
    .into_iter()
    .enumerate()
    {
        let base = passage(seed, index);
        let variants = [
            ("ours", base.clone()),
            ("auphonic", brighter(&base)),
            ("untreated", noisier(&base, seed)),
        ];

        let mut labels = BTreeMap::new();
        let mut clips = Vec::new();
        // Rotated per trial rather than shuffled, so the same variant is not
        // clip A every time and the ordering is still reproducible.
        for (position, (variant, signal)) in variants.iter().enumerate() {
            let label = ["A", "B", "C"][(position + index) % 3].to_string();
            let file = format!("{id}_{label}.wav");
            write_wav(&dir.join(&file), signal)?;
            labels.insert(label.clone(), (*variant).to_string());
            clips.push(Clip { label, file });
        }
        clips.sort_by(|a, b| a.label.cmp(&b.label));

        trials.push(Trial {
            id: id.to_string(),
            title: title.to_string(),
            clips,
        });
        clips_by_trial.insert(id.to_string(), labels);
        windows.insert(
            id.to_string(),
            Window {
                start: 0.0,
                dur: 12.0,
                label: None,
            },
        );
    }

    let session = Session {
        name: name.to_string(),
        created_at: store::now(),
        loudness_lufs: -23.0,
        match_by: MatchBy::MomentaryMax,
        clip_questions: default_clip_questions(),
        trial_questions: default_trial_questions(),
        trials,
    };
    write_json(&dir.join("session.json"), &session)?;

    let key = SessionKey {
        name: name.to_string(),
        variants: [
            ("ours", "the chain in this repository"),
            ("auphonic", "Auphonic, adaptive levelling"),
            ("untreated", "straight off the recorder"),
        ]
        .into_iter()
        .map(|(name, description)| {
            (
                name.to_string(),
                leveller_listen::KeyVariant {
                    file: format!("{name}.wav"),
                    description: Some(description.to_string()),
                },
            )
        })
        .collect(),
        clips: clips_by_trial,
        windows,
    };
    write_json(&dir.join("key.json"), &key)?;

    // Something to annotate: a long reading with clicks in it.
    let reading = clicky();
    write_wav(&root.join("annotate").join("reading.wav"), &reading)?;

    println!("wrote {} ", root.display());
    Ok(())
}

/// A passage: a few spurts at different levels with pauses between them.
fn passage(seed: u32, index: usize) -> Signal {
    leveller_corpus::synthetic_speech(&SpeechOptions {
        sample_rate: 48_000,
        spurts: vec![
            Spurt::new(3.5, -24.0 + index as f64).at_pitch(112.0),
            Spurt::new(2.5, -19.0 + index as f64).at_pitch(128.0),
            Spurt::new(4.0, -27.0 + index as f64).at_pitch(104.0),
        ],
        pause_sec: 0.6,
        floor_dbfs: -58.0,
        seed,
        channels: 1,
    })
    .signal
}

/// A treatment that lifted the top end, which is what "harsh" sounds like.
fn brighter(signal: &Signal) -> Signal {
    let mut out = signal.clone();
    for channel in out.channels_mut() {
        let mut previous = 0.0f32;
        for sample in channel.iter_mut() {
            let high = *sample - previous;
            previous = *sample;
            *sample = (*sample + high * 0.7).clamp(-1.0, 1.0);
        }
    }
    out
}

/// A treatment that did nothing: the room is still in it.
fn noisier(signal: &Signal, seed: u32) -> Signal {
    let reverberant = add_reverb(
        signal,
        &RirOptions {
            rt60_sec: 0.55,
            direct_to_reverb_db: 9.0,
            pre_delay_sec: 0.012,
            seed,
        },
    );
    leveller_corpus::add_noise(&reverberant, 28.0, seed)
}

/// A reading with clicks in it, to mark up.
fn clicky() -> Signal {
    let speech = leveller_corpus::synthetic_speech(&SpeechOptions {
        sample_rate: 48_000,
        spurts: (0..8)
            .map(|i| Spurt::new(2.0 + f64::from(i % 3), -22.0 - f64::from(i % 4)))
            .collect(),
        pause_sec: 0.45,
        floor_dbfs: -60.0,
        seed: 99,
        channels: 1,
    })
    .signal;

    add_clicks(
        &speech,
        &ClickOptions {
            count: 24,
            relative_amplitude: 0.9,
            width_samples: 6,
            min_gap_sec: 0.4,
            seed: 5,
        },
    )
    .signal
}

fn write_wav(path: &Path, signal: &Signal) -> std::io::Result<()> {
    let audio = leveller_wav::Audio {
        signal: signal.clone(),
        bit_depth: 24,
        format: leveller_wav::SampleFormat::Int,
    };
    std::fs::write(
        path,
        leveller_wav::encode(&audio).map_err(std::io::Error::other)?,
    )
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)
}
