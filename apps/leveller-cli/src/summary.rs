//! The text summary: what the chain decided, in a form a person can read.
//!
//! Rendered to a string rather than printed, so it can be tested without
//! capturing stdout.

use std::fmt::Write as _;

use leveller_io::{ProcessResult, leveller_report};
use serde_json::Value;

fn plural(n: usize, singular: &str) -> String {
    if n == 1 {
        format!("{n} {singular}")
    } else {
        format!("{n} {singular}s")
    }
}

fn signed(value: f64) -> String {
    format!("{}{value:.1}", if value >= 0.0 { "+" } else { "" })
}

fn lufs(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.1} LUFS")
    } else {
        "silent".into()
    }
}

fn dbfs(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.1} dBFS")
    } else {
        "silent".into()
    }
}

pub fn render(result: &ProcessResult) -> String {
    let report = &result.report;
    let mut out = String::new();

    let channels = match report.channels {
        1 => "Mono".to_string(),
        n => format!("{n} ch"),
    };
    let resampled = if report.resampled {
        format!(" (chain ran at {} Hz)", report.working_sample_rate)
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "{channels} · {} Hz · {:.1}s{resampled}",
        report.source_sample_rate, report.duration_sec
    );
    let _ = writeln!(
        out,
        "Input:  {}, peak {}",
        lufs(report.input.integrated_lufs),
        dbfs(report.input.peak_dbfs)
    );
    let _ = writeln!(
        out,
        "Output: {}, peak {}",
        lufs(report.output.integrated_lufs),
        dbfs(report.output.peak_dbfs)
    );

    let _ = writeln!(out, "\nChain:");
    for stage in &report.stages {
        if stage.enabled {
            let _ = writeln!(out, "  {}  ({:.0} ms)", stage.name, stage.elapsed_ms);
        } else {
            let _ = writeln!(out, "  {}  – bypassed", stage.name);
        }
    }

    if let Some(leveller) = leveller_report(report) {
        render_leveller(&mut out, leveller, result);
    }

    let _ = writeln!(out, "\nWrote {}", result.output_path.display());
    for (_, path) in &result.extra_paths {
        let _ = writeln!(out, "Wrote {}", path.display());
    }
    out
}

fn render_leveller(out: &mut String, leveller: &Value, result: &ProcessResult) {
    let number = |value: &Value| value.as_f64().unwrap_or(f64::NAN);
    let sample_rate = number(&leveller["sampleRate"]).max(1.0);
    let seconds = |samples: f64| samples / sample_rate;

    let _ = writeln!(
        out,
        "\nSilence threshold: {:.1} LUFS (floor {:.1})",
        number(&leveller["thresholdLufs"]),
        number(&leveller["floorLufs"])
    );

    let silences = leveller["silences"].as_array().map_or(0, Vec::len);
    let segments = leveller["segments"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(
        out,
        "Detected {}, {}:",
        plural(silences, "silence"),
        plural(segments.len(), "segment")
    );
    for (i, segment) in segments.iter().enumerate() {
        let _ = writeln!(
            out,
            "  #{}  {:.2}s–{:.2}s  {}  →  gain {} dB",
            i + 1,
            seconds(number(&segment["start"])),
            seconds(number(&segment["end"])),
            lufs(number(&segment["loudnessLufs"])),
            signed(number(&segment["gainDb"]))
        );
    }

    let limiter = number(&leveller["limiterGainReductionDb"]);
    if limiter > 0.01 {
        let _ = writeln!(out, "Limiter engaged: up to {limiter:.1} dB reduction");
    }

    let bed = &leveller["roomTone"];
    if result.room_tone_path.is_some() {
        let _ = writeln!(
            out,
            "Room tone: {:.1}s from {} ({} looped), gain {} dB",
            number(&bed["durationSec"]),
            plural(bed["clips"].as_array().map_or(0, Vec::len), "clean clip"),
            number(&bed["instances"]),
            signed(number(&bed["gainDb"]))
        );
    } else {
        let _ = writeln!(out, "Room tone: skipped (no usable silence found)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_pipeline::PipelineReport;
    use serde_json::json;
    use std::path::PathBuf;

    fn report() -> PipelineReport {
        serde_json::from_value(json!({
            "sourceSampleRate": 48_000,
            "workingSampleRate": 48_000,
            "channels": 1,
            "durationSec": 12.5,
            "resampled": false,
            "input": { "integratedLufs": -27.4, "peakDbfs": -6.2 },
            "output": { "integratedLufs": -18.0, "peakDbfs": -1.0 },
            "extras": ["roomtone"],
            "stages": [
                { "name": "declick", "enabled": true, "params": {}, "elapsedMs": 41.2, "report": {} },
                { "name": "denoise", "enabled": false, "params": {}, "elapsedMs": 0.0, "report": null },
                {
                    "name": "level",
                    "enabled": true,
                    "params": {},
                    "elapsedMs": 120.9,
                    "report": {
                        "sampleRate": 48_000,
                        "thresholdLufs": -46.2,
                        "floorLufs": -61.0,
                        "limiterGainReductionDb": 2.4,
                        "silences": [{ "start": 0, "end": 48_000, "mid": 24_000 }],
                        "segments": [
                            { "start": 0, "end": 24_000, "loudnessLufs": -31.0, "gainDb": 13.0, "isSpeech": true },
                            { "start": 24_000, "end": 96_000, "loudnessLufs": -22.0, "gainDb": 4.0, "isSpeech": true }
                        ],
                        "roomTone": {
                            "clips": [{ "start": 0, "end": 48_000, "score": -50.0 }],
                            "instances": 12,
                            "gainDb": 8.5,
                            "length": 480_000,
                            "durationSec": 10.0
                        }
                    }
                }
            ]
        }))
        .expect("the fixture should parse")
    }

    fn result(with_bed: bool) -> ProcessResult {
        ProcessResult {
            input_path: PathBuf::from("/tmp/talk.wav"),
            output_path: PathBuf::from("/tmp/talk_processed.wav"),
            room_tone_path: with_bed.then(|| PathBuf::from("/tmp/talk_roomtone.wav")),
            extra_paths: if with_bed {
                vec![("roomtone".into(), PathBuf::from("/tmp/talk_roomtone.wav"))]
            } else {
                Vec::new()
            },
            report: report(),
        }
    }

    #[test]
    fn the_header_says_what_the_file_is() {
        let text = render(&result(true));
        assert!(text.contains("Mono · 48000 Hz · 12.5s"), "{text}");
        assert!(
            text.contains("Input:  -27.4 LUFS, peak -6.2 dBFS"),
            "{text}"
        );
        assert!(
            text.contains("Output: -18.0 LUFS, peak -1.0 dBFS"),
            "{text}"
        );
    }

    #[test]
    fn a_bypassed_stage_is_shown_as_bypassed_rather_than_as_instant() {
        let text = render(&result(true));
        assert!(text.contains("declick  (41 ms)"), "{text}");
        assert!(text.contains("denoise  – bypassed"), "{text}");
    }

    #[test]
    fn every_segment_is_listed_with_what_it_was_given() {
        let text = render(&result(true));
        assert!(text.contains("Detected 1 silence, 2 segments:"), "{text}");
        assert!(
            text.contains("#1  0.00s–0.50s  -31.0 LUFS  →  gain +13.0 dB"),
            "{text}"
        );
        assert!(
            text.contains("#2  0.50s–2.00s  -22.0 LUFS  →  gain +4.0 dB"),
            "{text}"
        );
    }

    #[test]
    fn the_limiter_is_mentioned_only_when_it_did_something() {
        assert!(render(&result(true)).contains("Limiter engaged: up to 2.4 dB"));

        let mut quiet = result(true);
        quiet.report.stages[2].report["limiterGainReductionDb"] = json!(0.0);
        assert!(!render(&quiet).contains("Limiter"));
    }

    #[test]
    fn the_bed_is_described_when_there_is_one_and_explained_when_there_is_not() {
        let with = render(&result(true));
        assert!(
            with.contains("Room tone: 10.0s from 1 clean clip (12 looped), gain +8.5 dB"),
            "{with}"
        );

        let without = render(&result(false));
        assert!(
            without.contains("Room tone: skipped (no usable silence found)"),
            "{without}"
        );
    }

    #[test]
    fn every_file_written_is_named() {
        let text = render(&result(true));
        assert!(text.contains("Wrote /tmp/talk_processed.wav"), "{text}");
        assert!(text.contains("Wrote /tmp/talk_roomtone.wav"), "{text}");
    }

    #[test]
    fn a_resampled_run_says_what_rate_it_ran_at() {
        let mut resampled = result(true);
        resampled.report.resampled = true;
        resampled.report.working_sample_rate = 48_000;
        resampled.report.source_sample_rate = 44_100;
        assert!(
            render(&resampled).contains("44100 Hz · 12.5s (chain ran at 48000 Hz)"),
            "{}",
            render(&resampled)
        );
    }

    #[test]
    fn silence_is_called_silent_rather_than_shown_as_minus_infinity() {
        let mut silent = result(true);
        silent.report.input.integrated_lufs = f64::NEG_INFINITY;
        silent.report.input.peak_dbfs = f64::NEG_INFINITY;
        let text = render(&silent);
        assert!(text.contains("Input:  silent, peak silent"), "{text}");
        assert!(!text.contains("inf"), "{text}");
    }

    #[test]
    fn a_report_with_no_leveller_in_it_still_renders() {
        let mut without = result(true);
        without.report.stages.truncate(2);
        let text = render(&without);
        assert!(!text.contains("Silence threshold"));
        assert!(text.contains("Wrote"));
    }
}
