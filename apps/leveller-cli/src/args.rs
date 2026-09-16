//! Argument parsing.
//!
//! Hand-rolled rather than delegated to a crate. The flag set is small and
//! fixed, the error messages are worth writing by hand, and a parser this size
//! is less code than the derive macro's attributes would be.

use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: audio-leveller <input.wav> [output.wav] [options]

Options:
  --only <a,b>      run only these stages (in chain order)
  --bypass <a,b>    run the chain but bypass these stages
  --preset <name>   start from a preset's parameters (default Nino)
  --target <lufs>   target loudness for the level stage (overrides the preset)
  --report <file>   write the full JSON report to a file
  --json            print the JSON report instead of the text summary
  --quiet           suppress progress output
  --list-stages     list the available stages and exit
  --list-presets    list the available presets and exit
  -h, --help        show this help";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub input_path: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub only: Vec<String>,
    pub bypass: Vec<String>,
    pub preset: Option<String>,
    pub target_lufs: Option<f64>,
    pub report_path: Option<PathBuf>,
    pub json: bool,
    pub quiet: bool,
    pub list_stages: bool,
    pub list_presets: bool,
    pub help: bool,
}

pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Args, String> {
    let mut args = Args::default();
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut argv = argv.into_iter();

    while let Some(arg) = argv.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            argv.next().ok_or_else(|| format!("{flag} needs a value"))
        };
        let list = |raw: String| -> Vec<String> {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };

        match arg.as_str() {
            // A bare `--` is the conventional argument separator, and some task
            // runners insert one. Skip it rather than rejecting it as a flag.
            "--" => {}
            "--only" => args.only = list(value("--only")?),
            "--bypass" => args.bypass = list(value("--bypass")?),
            "--preset" => args.preset = Some(value("--preset")?),
            "--target" => {
                let raw = value("--target")?;
                args.target_lufs = Some(
                    raw.parse()
                        .map_err(|_| "--target needs a number in LUFS".to_string())?,
                );
            }
            "--report" => args.report_path = Some(PathBuf::from(value("--report")?)),
            "--json" => args.json = true,
            "--quiet" => args.quiet = true,
            "--list-stages" => args.list_stages = true,
            "--list-presets" => args.list_presets = true,
            "-h" | "--help" => args.help = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option \"{other}\""));
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }

    let mut positional = positional.into_iter();
    args.input_path = positional.next();
    args.output_path = positional.next();
    if let Some(extra) = positional.next() {
        return Err(format!("unexpected argument \"{}\"", extra.display()));
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn an_input_alone_is_enough() {
        let args = parse_of(&["talk.wav"]).unwrap();
        assert_eq!(args.input_path, Some(PathBuf::from("talk.wav")));
        assert_eq!(args.output_path, None);
        assert!(!args.json && !args.quiet);
    }

    #[test]
    fn an_output_can_follow_the_input() {
        let args = parse_of(&["in.wav", "out.wav"]).unwrap();
        assert_eq!(args.input_path, Some(PathBuf::from("in.wav")));
        assert_eq!(args.output_path, Some(PathBuf::from("out.wav")));
    }

    #[test]
    fn stage_lists_are_split_and_trimmed() {
        let args = parse_of(&["in.wav", "--bypass", "denoise, dereverb ,eq"]).unwrap();
        assert_eq!(args.bypass, vec!["denoise", "dereverb", "eq"]);

        let empty = parse_of(&["in.wav", "--only", " , ,"]).unwrap();
        assert!(empty.only.is_empty());
    }

    #[test]
    fn the_target_must_be_a_number() {
        assert_eq!(
            parse_of(&["in.wav", "--target", "-23"])
                .unwrap()
                .target_lufs,
            Some(-23.0)
        );
        assert!(parse_of(&["in.wav", "--target", "loud"]).is_err());
    }

    #[test]
    fn a_flag_missing_its_value_says_which_flag() {
        let error = parse_of(&["in.wav", "--preset"]).unwrap_err();
        assert!(error.contains("--preset"), "{error}");
    }

    #[test]
    fn an_unknown_option_is_refused_rather_than_read_as_a_filename() {
        let error = parse_of(&["in.wav", "--lovely"]).unwrap_err();
        assert!(error.contains("--lovely"), "{error}");
    }

    #[test]
    fn a_bare_double_dash_is_skipped() {
        // `pnpm run x -- --flag` and its relatives insert one.
        let args = parse_of(&["--", "in.wav", "--quiet"]).unwrap();
        assert_eq!(args.input_path, Some(PathBuf::from("in.wav")));
        assert!(args.quiet);
    }

    #[test]
    fn the_listing_flags_need_no_input() {
        assert!(parse_of(&["--list-stages"]).unwrap().list_stages);
        assert!(parse_of(&["--list-presets"]).unwrap().list_presets);
        assert!(parse_of(&["-h"]).unwrap().help);
        assert!(parse_of(&["--help"]).unwrap().help);
    }

    #[test]
    fn a_third_filename_is_an_error_rather_than_ignored() {
        let error = parse_of(&["a.wav", "b.wav", "c.wav"]).unwrap_err();
        assert!(error.contains("c.wav"), "{error}");
    }

    #[test]
    fn a_negative_target_is_not_read_as_an_option() {
        // The one place a leading dash is a value rather than a flag.
        let args = parse_of(&["in.wav", "--target", "-18.5"]).unwrap();
        assert_eq!(args.target_lufs, Some(-18.5));
    }
}
