//! The command line.
//!
//! Handy for batch use, for eyeballing what the chain decided, and — via
//! `--bypass` with `--report` — for comparing a stage against itself when
//! tuning by ear.

mod args;
mod summary;

use std::process::ExitCode;

use leveller_io::{output_path_for, process_file_to};
use leveller_pipeline::Progress;
use leveller_stages::{ChainOptions, DEFAULT_CHAIN, build_chain, default_registry, params};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = args::parse(std::env::args().skip(1))?;
    let registry = default_registry();

    if args.help {
        println!("{}", args::USAGE);
        return Ok(());
    }

    if args.list_stages {
        for stage in registry.stages() {
            let note = if DEFAULT_CHAIN.contains(&stage.name()) {
                ""
            } else {
                "  (not in default chain)"
            };
            println!("{:<12} {}{note}", stage.name(), stage.description());
        }
        return Ok(());
    }

    if args.list_presets {
        for preset in params::presets() {
            println!("{:<12} {}", preset.name, preset.description);
        }
        return Ok(());
    }

    let Some(input_path) = args.input_path.as_deref() else {
        return Err(args::USAGE.to_string());
    };
    let output_path = args
        .output_path
        .clone()
        .unwrap_or_else(|| output_path_for(input_path));

    // A preset is only a set of overrides, so --target layers on top of it
    // rather than fighting it.
    let mut chain = ChainOptions {
        only: args.only.clone(),
        bypass: args.bypass.clone(),
        params: Vec::new(),
    };
    if let Some(name) = &args.preset {
        let preset = params::find_preset(name).ok_or_else(|| {
            format!(
                "unknown preset \"{name}\" (available: {})",
                params::presets()
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        chain
            .bypass
            .extend(preset.bypass.iter().map(|s| (*s).to_string()));
        chain.params = preset
            .params
            .iter()
            .map(|(stage, overrides)| ((*stage).to_string(), overrides.clone()))
            .collect();
    }
    if let Some(target) = args.target_lufs {
        let entry = match chain.params.iter_mut().find(|(stage, _)| stage == "level") {
            Some(entry) => entry,
            None => {
                chain.params.push(("level".into(), serde_json::Map::new()));
                chain.params.last_mut().expect("just pushed")
            }
        };
        entry.1.insert("targetLufs".into(), target.into());
    }

    let stages = build_chain(&chain).map_err(|e| e.to_string())?;

    let show_progress = !args.quiet && !args.json;
    let mut last_stage = String::new();
    let result = process_file_to(
        input_path,
        &output_path,
        &stages,
        &registry,
        |p: Progress| {
            if !show_progress || p.stage == last_stage {
                return;
            }
            last_stage = p.stage.to_string();
            eprintln!("[{}/{}] {}...", p.index + 1, p.total, p.stage);
        },
    )
    .map_err(|e| e.to_string())?;

    if let Some(path) = &args.report_path {
        let json = serde_json::to_string_pretty(&result.report)
            .map_err(|e| format!("could not render the report: {e}"))?;
        std::fs::write(path, json)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result.report)
                .map_err(|e| format!("could not render the report: {e}"))?
        );
    } else {
        print!("{}", summary::render(&result));
    }

    Ok(())
}
