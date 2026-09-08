//! Write a synthetic recording, for trying the chain without a real one to hand.
//!
//! `cargo run -p leveller-cli --example make_fixture -- /tmp/talk.wav`

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixture.wav".into());
    let speech = leveller_corpus::synthetic_speech(&leveller_corpus::SpeechOptions {
        sample_rate: 48_000,
        spurts: vec![
            leveller_corpus::Spurt::new(6.0, -31.0),
            leveller_corpus::Spurt::new(6.0, -17.0),
            leveller_corpus::Spurt::new(6.0, -24.0),
        ],
        pause_sec: 2.0,
        floor_dbfs: -52.0,
        seed: 4242,
        channels: 1,
    });
    let noisy = leveller_corpus::add_noise(&speech.signal, 22.0, 5);
    let clicked = leveller_corpus::add_clicks(&noisy, &leveller_corpus::ClickOptions::default());

    let audio = leveller_wav::Audio {
        signal: leveller_dsp::Signal::new(48_000, clicked.signal.channels().to_vec()),
        bit_depth: 24,
        format: leveller_wav::SampleFormat::Int,
    };
    std::fs::write(&path, leveller_wav::encode(&audio).unwrap()).unwrap();
    println!("wrote {path}");
}
