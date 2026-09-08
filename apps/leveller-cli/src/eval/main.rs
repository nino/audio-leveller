//! The evaluation harness.
//!
//!   leveller-eval [options]
//!
//! Renders every case through the pipeline, measures the result, and checks it
//! against the case's stated bounds. Exits non-zero when a bound is broken, so
//! it works as a regression gate as well as a tuning tool.
//!
//! The workflow it exists for: run it, note the numbers, build a stage, run it
//! again with `--baseline` and see exactly what moved. "Sounds better" is not
//! evidence; a click residual that fell 45 dB while SI-SDR held steady is.

mod args;
mod report;

use std::process::ExitCode;

use leveller_eval::cases;
use leveller_eval::runner;


fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Returns whether every case that ran passed.
fn run() -> Result<bool, String> {
    let args = args::parse(std::env::args().skip(1))?;
    if args.help {
        println!("{}", args::USAGE);
        return Ok(true);
    }

    // The model backend included, so its cases run rather than being skipped
    // wherever the weights happen to be installed.
    let backends = leveller_model::backends();
    let mut cases = cases::all(&backends);
    cases.extend(
        runner::fixture_cases(&args.fixtures_dir, &backends).map_err(|e| e.to_string())?,
    );
    if let Some(filter) = &args.filter {
        cases.retain(|case| case.name.contains(filter));
        if cases.is_empty() {
            return Err(format!("no case matches \"{filter}\""));
        }
    }

    let registry = leveller_model::registry();
    // Printed as they finish rather than at the end: a full run is minutes of
    // DSP, and watching it go is the difference between a tool and a wait.
    let quiet = args.json;
    let results = runner::run_all(
        &cases,
        &registry,
        |result| {
            if !quiet {
                print!("{}", report::case(result, args.verbose));
            }
        },
        args.wav_dir.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    if args.json {
        println!("{}", report::json(&results));
    }

    if let Some(path) = &args.baseline_path {
        let baseline = runner::read_baseline(path).map_err(|e| e.to_string())?;
        print!("{}", report::baseline_diff(&results, &baseline));
    }
    if let Some(path) = &args.save_baseline_path {
        runner::write_baseline(path, &runner::baseline_of(&results)).map_err(|e| e.to_string())?;
        println!("\nWrote baseline to {}", path.display());
    }
    if let Some(path) = &args.out_path {
        std::fs::write(path, report::json(&results))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("Wrote results to {}", path.display());
    }
    if let Some(dir) = &args.wav_dir {
        println!("Wrote rendered audio to {}", dir.display());
    }

    let failed = results.iter().filter(|r| !r.passed).count();
    let skipped = results.iter().filter(|r| r.skipped.is_some()).count();
    if !args.json {
        println!("{}", report::summary(&results, failed, skipped));
    }
    Ok(failed == 0)
}

/// Every case that actually ran, for the tests.
#[cfg(test)]
fn ran(results: &[runner::CaseResult]) -> Vec<&runner::CaseResult> {
    results.iter().filter(|r| r.skipped.is_none()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole corpus, run once and shared: it is minutes of DSP and every
    /// test here asks a different question of the same answers.
    fn results() -> &'static Vec<runner::CaseResult> {
        static RESULTS: std::sync::OnceLock<Vec<runner::CaseResult>> = std::sync::OnceLock::new();
        RESULTS.get_or_init(|| {
            // The classical backend only: the model cases are minutes of
            // inference and depend on weights that are not in the repository,
            // so the test suite runs the part that is the same everywhere and
            // the command runs everything.
            let backends = leveller_stages::default_backends();
            let cases = cases::all(&backends);
            runner::run_all(&cases, &leveller_stages::default_registry(), |_| {}, None)
                .expect("the corpus runs")
        })
    }

    #[test]
    fn every_case_meets_its_bounds() {
        // The regression gate, as a test as well as a command: a stage that
        // breaks a bound fails `cargo test` rather than waiting for somebody to
        // remember to run the harness.
        let failures: Vec<String> = results()
            .iter()
            .filter(|r| !r.passed)
            .flat_map(|r| {
                r.checks
                    .iter()
                    .filter(|c| !c.passed)
                    .map(move |c| {
                        format!(
                            "{}: {} = {:.2} (min {:?}, max {:?})\n    {}",
                            r.name,
                            c.expectation.metric,
                            c.value,
                            c.expectation.min,
                            c.expectation.max,
                            c.expectation.because
                        )
                    })
            })
            .collect();
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn the_corpus_actually_ran() {
        // A harness that skipped everything would pass the test above.
        assert!(
            ran(results()).len() >= 20,
            "only {} cases ran",
            ran(results()).len()
        );
    }
}
