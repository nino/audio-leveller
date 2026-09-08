//! Read a file, run the chain, write the results.
//!
//! Shared by the app and the command line so both use identical logic. Any
//! extra signal a stage produces — currently the room-tone bed — is written as
//! `<name>_<key>.wav` next to the input, so a stage added later gets an output
//! file without anything here changing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leveller_pipeline::{PipelineError, PipelineReport, Progress, Registry, StageSpec, run};
use leveller_wav::{Audio, WavError};

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not audio this can read: {source}")]
    Decode { path: PathBuf, source: WavError },
    #[error("could not encode the result: {0}")]
    Encode(#[from] WavError),
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
}

/// The `<name>_<suffix>.wav` path next to an input.
pub fn sibling_path(input: &Path, suffix: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    input.with_file_name(format!("{stem}_{suffix}.wav"))
}

/// Where the processed file goes.
pub fn output_path_for(input: &Path) -> PathBuf {
    sibling_path(input, "processed")
}

#[derive(Debug)]
pub struct ProcessResult {
    pub input_path: PathBuf,
    pub output_path: PathBuf,
    /// Path of the room-tone bed, or `None` when no usable silence was found.
    pub room_tone_path: Option<PathBuf>,
    /// Every extra signal written, keyed by its stage-declared name.
    pub extra_paths: Vec<(String, PathBuf)>,
    pub report: PipelineReport,
}

/// Run a file through a chain and write what comes out.
pub fn process_file(
    input_path: &Path,
    stages: &[StageSpec],
    registry: &Registry,
    on_progress: impl FnMut(Progress),
) -> Result<ProcessResult, ProcessError> {
    process_file_to(
        input_path,
        &output_path_for(input_path),
        stages,
        registry,
        on_progress,
    )
}

/// [`process_file`] with the output path chosen by the caller.
pub fn process_file_to(
    input_path: &Path,
    output_path: &Path,
    stages: &[StageSpec],
    registry: &Registry,
    on_progress: impl FnMut(Progress),
) -> Result<ProcessResult, ProcessError> {
    let bytes = std::fs::read(input_path).map_err(|source| ProcessError::Read {
        path: input_path.to_path_buf(),
        source,
    })?;
    let audio = leveller_wav::decode(&bytes).map_err(|source| ProcessError::Decode {
        path: input_path.to_path_buf(),
        source,
    })?;

    let result = run(
        &Arc::new(audio.signal.clone()),
        stages,
        registry,
        on_progress,
    )?;

    // The file's own bit depth and format travel with the audio, so a 24-bit
    // recording comes back 24-bit.
    write_wav(output_path, &Audio::like((*result.signal).clone(), &audio))?;

    let mut extra_paths = Vec::new();
    // Sorted so the order of files written does not depend on a hash map's
    // iteration order, which is a small thing that makes runs reproducible.
    let mut extras: Vec<_> = result.extras.into_iter().collect();
    extras.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, signal) in extras {
        let path = sibling_path(input_path, &key);
        write_wav(&path, &Audio::like(signal, &audio))?;
        extra_paths.push((key, path));
    }

    Ok(ProcessResult {
        input_path: input_path.to_path_buf(),
        output_path: output_path.to_path_buf(),
        room_tone_path: extra_paths
            .iter()
            .find(|(key, _)| key == "roomtone")
            .map(|(_, path)| path.clone()),
        extra_paths,
        report: result.report,
    })
}

fn write_wav(path: &Path, audio: &Audio) -> Result<(), ProcessError> {
    let bytes = leveller_wav::encode(audio)?;
    std::fs::write(path, bytes).map_err(|source| ProcessError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// The leveller's own report, pulled out of a pipeline report, when it ran.
pub fn leveller_report(report: &PipelineReport) -> Option<&serde_json::Value> {
    report
        .stages
        .iter()
        .find(|s| s.name == "level" && s.enabled)
        .map(|s| &s.report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_corpus::{SpeechOptions, Spurt, synthetic_speech};
    use leveller_dsp::Signal;
    use leveller_stages::{ChainOptions, build_chain, default_registry};
    use leveller_wav::SampleFormat;

    /// A temporary directory that cleans up after itself.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "leveller-io-{name}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_input(path: &Path, bit_depth: u16, format: SampleFormat) -> Signal {
        let speech = synthetic_speech(&SpeechOptions {
            sample_rate: 48_000,
            spurts: vec![Spurt::new(3.0, -30.0), Spurt::new(3.0, -18.0)],
            pause_sec: 1.5,
            floor_dbfs: -60.0,
            seed: 4242,
            channels: 1,
        });
        let signal = Signal::new(48_000, speech.signal.channels().to_vec());
        let audio = Audio {
            signal: signal.clone(),
            bit_depth,
            format,
        };
        std::fs::write(path, leveller_wav::encode(&audio).unwrap()).unwrap();
        signal
    }

    fn chain() -> Vec<StageSpec> {
        build_chain(&ChainOptions::default()).unwrap()
    }

    #[test]
    fn a_file_goes_in_and_two_come_out() {
        let scratch = Scratch::new("basic");
        let input = scratch.join("talk.wav");
        write_input(&input, 24, SampleFormat::Int);

        let result =
            process_file(&input, &chain(), &default_registry(), |_| {}).expect("processing");

        assert_eq!(result.output_path, scratch.join("talk_processed.wav"));
        assert!(result.output_path.exists());
        assert_eq!(
            result.room_tone_path,
            Some(scratch.join("talk_roomtone.wav"))
        );
        assert!(result.room_tone_path.unwrap().exists());
    }

    #[test]
    fn the_output_keeps_the_input_s_bit_depth_and_format() {
        // A 24-bit recording should not quietly become 16-bit because it
        // passed through here.
        for (bit_depth, format) in [
            (16, SampleFormat::Int),
            (24, SampleFormat::Int),
            (32, SampleFormat::Float),
        ] {
            let scratch = Scratch::new("depth");
            let input = scratch.join("talk.wav");
            write_input(&input, bit_depth, format);

            let result =
                process_file(&input, &chain(), &default_registry(), |_| {}).expect("processing");
            let written = leveller_wav::decode(&std::fs::read(&result.output_path).unwrap())
                .expect("the output should be readable");

            assert_eq!(written.bit_depth, bit_depth);
            assert_eq!(written.format, format);
            assert_eq!(written.signal.sample_rate(), 48_000);
        }
    }

    #[test]
    fn the_result_is_actually_levelled() {
        let scratch = Scratch::new("levelled");
        let input = scratch.join("talk.wav");
        write_input(&input, 24, SampleFormat::Int);

        let result =
            process_file(&input, &chain(), &default_registry(), |_| {}).expect("processing");
        let written = leveller_wav::decode(&std::fs::read(&result.output_path).unwrap()).unwrap();

        let measured = leveller_dsp::loudness::Weighted::new(
            written.signal.channels(),
            written.signal.sample_rate(),
        )
        .integrated();
        assert!((measured - -18.0).abs() < 1.5, "{measured} LUFS");
    }

    #[test]
    fn the_report_comes_back_with_the_files() {
        let scratch = Scratch::new("report");
        let input = scratch.join("talk.wav");
        write_input(&input, 24, SampleFormat::Int);

        let result =
            process_file(&input, &chain(), &default_registry(), |_| {}).expect("processing");
        assert_eq!(result.report.stages.len(), 8);
        assert_eq!(result.report.extras, vec!["roomtone".to_string()]);
        assert!(leveller_report(&result.report).is_some());
    }

    #[test]
    fn a_bypassed_chain_writes_the_file_back_unchanged() {
        let scratch = Scratch::new("bypass");
        let input = scratch.join("talk.wav");
        write_input(&input, 24, SampleFormat::Int);
        // Compared against the file on disk, not against the signal that was
        // written: 24-bit quantisation happens on the way in, and the chain is
        // not responsible for that.
        let original = leveller_wav::decode(&std::fs::read(&input).unwrap())
            .unwrap()
            .signal;

        let specs = build_chain(&ChainOptions {
            bypass: leveller_stages::DEFAULT_CHAIN
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..ChainOptions::default()
        })
        .unwrap();
        let result = process_file(&input, &specs, &default_registry(), |_| {}).expect("processing");

        let written = leveller_wav::decode(&std::fs::read(&result.output_path).unwrap()).unwrap();
        assert_eq!(written.signal.len(), original.len());
        let changed = written
            .signal
            .channel(0)
            .iter()
            .zip(original.channel(0))
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(changed, 0, "a bypassed chain changed {changed} samples");
        assert!(result.room_tone_path.is_none(), "and produced no bed");
    }

    #[test]
    fn progress_is_reported_while_it_runs() {
        let scratch = Scratch::new("progress");
        let input = scratch.join("talk.wav");
        write_input(&input, 16, SampleFormat::Int);

        let mut overall: Vec<f64> = Vec::new();
        process_file(&input, &chain(), &default_registry(), |p| {
            overall.push(p.overall)
        })
        .expect("processing");

        assert_eq!(overall.first(), Some(&0.0));
        assert_eq!(overall.last(), Some(&1.0));
    }

    #[test]
    fn the_output_path_can_be_chosen() {
        let scratch = Scratch::new("explicit");
        let input = scratch.join("talk.wav");
        write_input(&input, 16, SampleFormat::Int);
        let output = scratch.join("somewhere-else.wav");

        let result = process_file_to(&input, &output, &chain(), &default_registry(), |_| {})
            .expect("processing");
        assert_eq!(result.output_path, output);
        assert!(output.exists());
        // The extras still land next to the *input*, which is where someone
        // looking for the bed would look.
        assert_eq!(
            result.room_tone_path,
            Some(scratch.join("talk_roomtone.wav"))
        );
    }

    #[test]
    fn a_missing_file_says_which_one() {
        let error = process_file(
            Path::new("/nowhere/at/all.wav"),
            &chain(),
            &default_registry(),
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(error, ProcessError::Read { .. }), "{error}");
        assert!(error.to_string().contains("/nowhere/at/all.wav"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_audio_says_so() {
        let scratch = Scratch::new("nonsense");
        let input = scratch.join("notes.txt");
        std::fs::write(&input, b"this is not a wav file").unwrap();

        let error = process_file(&input, &chain(), &default_registry(), |_| {}).unwrap_err();
        assert!(matches!(error, ProcessError::Decode { .. }), "{error}");
    }

    #[test]
    fn sibling_paths_are_built_from_the_stem() {
        assert_eq!(
            sibling_path(Path::new("/tmp/episode 12.wav"), "processed"),
            PathBuf::from("/tmp/episode 12_processed.wav")
        );
        assert_eq!(
            output_path_for(Path::new("talk.wav")),
            PathBuf::from("talk_processed.wav")
        );
        // A path with no stem still produces something writable rather than
        // panicking.
        assert_eq!(
            sibling_path(Path::new("/tmp/"), "roomtone"),
            PathBuf::from("/tmp_roomtone.wav")
        );
    }
}
