//! Argument parsing for the harness, in the same hand-rolled style as the
//! leveller's own.

use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: leveller-eval [options]

Options:
  --case <substring>     run only cases whose name contains this
  --fixtures <dir>       directory of real .wav files to include (default eval/fixtures)
  --baseline <file>      compare against a saved results file and show what moved
  --save-baseline <file> write this run's metrics as the new baseline
  --out <file>           write full results as JSON
  --wav <dir>            dump each case's input and output as .wav for listening
  --verbose              print every metric, not just the ones with bounds
  --json                 print results as JSON
  -h, --help             show this help";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub filter: Option<String>,
    pub fixtures_dir: PathBuf,
    pub baseline_path: Option<PathBuf>,
    pub save_baseline_path: Option<PathBuf>,
    pub out_path: Option<PathBuf>,
    pub wav_dir: Option<PathBuf>,
    pub verbose: bool,
    pub json: bool,
    pub help: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            filter: None,
            fixtures_dir: PathBuf::from("eval/fixtures"),
            baseline_path: None,
            save_baseline_path: None,
            out_path: None,
            wav_dir: None,
            verbose: false,
            json: false,
            help: false,
        }
    }
}

pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Args, String> {
    let mut args = Args::default();
    let mut argv = argv.into_iter();

    while let Some(arg) = argv.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            argv.next().ok_or_else(|| format!("{flag} needs a value"))
        };

        match arg.as_str() {
            // A bare `--` is the conventional argument separator, and some task
            // runners insert one.
            "--" => {}
            "--case" => args.filter = Some(value("--case")?),
            "--fixtures" => args.fixtures_dir = PathBuf::from(value("--fixtures")?),
            "--baseline" => args.baseline_path = Some(PathBuf::from(value("--baseline")?)),
            "--save-baseline" => {
                args.save_baseline_path = Some(PathBuf::from(value("--save-baseline")?));
            }
            "--out" => args.out_path = Some(PathBuf::from(value("--out")?)),
            "--wav" => args.wav_dir = Some(PathBuf::from(value("--wav")?)),
            "--verbose" => args.verbose = true,
            "--json" => args.json = true,
            "-h" | "--help" => args.help = true,
            other => return Err(format!("unknown option \"{other}\"")),
        }
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn no_arguments_runs_the_whole_corpus_against_the_default_fixtures() {
        let args = parse_str(&[]).unwrap();
        assert_eq!(args.filter, None);
        assert_eq!(args.fixtures_dir, PathBuf::from("eval/fixtures"));
    }

    #[test]
    fn a_flag_missing_its_value_says_which_one() {
        assert_eq!(
            parse_str(&["--baseline"]),
            Err("--baseline needs a value".into())
        );
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_ignored() {
        // Silently ignoring `--save-basline` would write no baseline and say
        // nothing about it.
        assert_eq!(
            parse_str(&["--save-basline", "x.json"]),
            Err("unknown option \"--save-basline\"".into())
        );
    }

    #[test]
    fn the_argument_separator_is_skipped() {
        assert!(parse_str(&["--", "--verbose"]).unwrap().verbose);
    }
}
