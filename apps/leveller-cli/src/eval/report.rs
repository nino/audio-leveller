//! Rendering a run.
//!
//! Every function returns a `String` rather than printing, so what the harness
//! says can be tested by calling it.

use leveller_eval::runner::{Baseline, CaseResult, diff};
use serde_json::{Map, Value, json};

/// How far a metric has to move before it is worth mentioning.
const THRESHOLD: f64 = 0.05;

fn fmt(n: f64) -> String {
    if n.is_nan() {
        return "n/a".into(); // a metric key that does not exist
    }
    if n.is_infinite() {
        return if n > 0.0 { "+inf" } else { "-inf" }.into();
    }
    format!("{n:.2}")
}

/// One case's line, its headline numbers, and any bound worth showing.
pub fn case(result: &CaseResult, verbose: bool) -> String {
    let mut out = String::new();
    if let Some(reason) = &result.skipped {
        out.push_str(&format!("– {:<22} {}\n", result.name, result.description));
        out.push_str(&format!("    skipped: {reason}\n"));
        return out;
    }

    let mark = if result.passed { "✓" } else { "✗" };
    out.push_str(&format!(
        "{mark} {:<22} {}\n",
        result.name, result.description
    ));

    let m = &result.metrics;
    let get = |key: &str| m.get(key).copied();
    let mut parts: Vec<String> = Vec::new();
    if let (Some(input), Some(output)) = (get("inputLufs"), get("outputLufs")) {
        parts.push(format!("{} → {} LUFS", fmt(input), fmt(output)));
    }
    if let Some(peak) = get("outputPeakDbfs") {
        parts.push(format!("peak {} dBFS", fmt(peak)));
    }
    if let Some(snr) = get("snrGainDb") {
        parts.push(format!("snr Δ {} dB", fmt(snr)));
    }
    // NaN where both scores were infinite — a stage that declined to act on a
    // reference identical to its input. There is nothing to report, so nothing
    // is reported rather than "n/a".
    if let Some(sdr) = get("siSdrGainDb").filter(|v| !v.is_nan()) {
        parts.push(format!("si-sdr Δ {} dB", fmt(sdr)));
    }
    if let Some(clicks) = get("outputClickResidualDb") {
        parts.push(format!("clicks {} dB", fmt(clicks)));
    }
    if !parts.is_empty() {
        out.push_str(&format!("    {}\n", parts.join(" · ")));
    }

    if verbose {
        for (key, value) in m {
            out.push_str(&format!("      {key:<24} {}\n", fmt(*value)));
        }
    }

    for check in &result.checks {
        if check.passed && !verbose {
            continue;
        }
        let bounds = [
            check.expectation.min.map(|min| format!("min {min}")),
            check.expectation.max.map(|max| format!("max {max}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        let mark = if check.passed { "  ok" } else { "FAIL" };
        out.push_str(&format!(
            "    {mark} {} = {} ({bounds})\n",
            check.expectation.metric,
            fmt(check.value)
        ));
        if !check.passed {
            out.push_str(&format!("         {}\n", check.expectation.because));
        }
    }
    out
}

pub fn baseline_diff(results: &[CaseResult], baseline: &Baseline) -> String {
    let mut out = format!("\nAgainst baseline (only changes over {THRESHOLD} dB shown):\n");
    let diffs = diff(results, baseline, THRESHOLD);
    if diffs.is_empty() {
        out.push_str("  nothing moved\n");
        return out;
    }
    for case in diffs {
        if case.is_new {
            out.push_str(&format!("  {}: new case, no baseline\n", case.name));
            continue;
        }
        out.push_str(&format!("  {}:\n", case.name));
        for moved in case.moves {
            let delta = moved.after - moved.before;
            let sign = if delta > 0.0 { "+" } else { "" };
            out.push_str(&format!(
                "    {} {} → {} ({sign}{})\n",
                moved.metric,
                fmt(moved.before),
                fmt(moved.after),
                fmt(delta)
            ));
        }
    }
    out
}

pub fn summary(results: &[CaseResult], failed: usize, skipped: usize) -> String {
    let total: f64 = results.iter().map(|r| r.elapsed_ms).sum::<f64>() / 1000.0;
    let ran = results.len() - skipped;
    let note = if skipped > 0 {
        format!(", {skipped} skipped")
    } else {
        String::new()
    };
    format!("\n{}/{ran} cases passed in {total:.1}s{note}", ran - failed)
}

pub fn json(results: &[CaseResult]) -> String {
    let values: Vec<Value> = results
        .iter()
        .map(|result| {
            let metrics: Map<String, Value> = result
                .metrics
                .iter()
                // JSON has no infinity, and a bypass case's `changeDb` is −∞ on
                // every run. `null` is the honest spelling of "no finite value"
                // and round-trips through the baseline as a missing key, which
                // the diff already treats as nothing to compare.
                .map(|(k, v)| (k.clone(), json!(v.is_finite().then_some(*v))))
                .collect();
            json!({
                "name": result.name,
                "description": result.description,
                "metrics": metrics,
                "passed": result.passed,
                "elapsedMs": result.elapsed_ms,
                "skipped": result.skipped,
                "checks": result.checks.iter().map(|c| json!({
                    "metric": c.expectation.metric,
                    "min": c.expectation.min,
                    "max": c.expectation.max,
                    "because": c.expectation.because,
                    "value": c.value.is_finite().then_some(c.value),
                    "passed": c.passed,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&values).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_eval::Check;
    use leveller_eval::cases::Expectation;

    fn result(name: &str, passed: bool) -> CaseResult {
        CaseResult {
            name: name.into(),
            description: "a case".into(),
            metrics: [
                ("inputLufs".to_string(), -30.0),
                ("outputLufs".to_string(), -18.0),
                ("changeDb".to_string(), f64::NEG_INFINITY),
            ]
            .into_iter()
            .collect(),
            checks: vec![Check {
                expectation: Expectation {
                    metric: "outputLufs",
                    min: None,
                    max: Some(-19.0),
                    because: "the reason this bound is the right one",
                },
                value: -18.0,
                passed,
            }],
            passed,
            elapsed_ms: 1200.0,
            skipped: None,
        }
    }

    #[test]
    fn a_passing_case_shows_its_numbers_and_not_its_bounds() {
        let text = case(&result("clean", true), false);
        assert!(text.contains("✓ clean"));
        assert!(text.contains("-30.00 → -18.00 LUFS"));
        assert!(!text.contains("outputLufs ="), "no need to show what held");
    }

    #[test]
    fn a_failing_case_shows_the_bound_and_the_reason() {
        // The reason is the point of writing it down: a failure that only says
        // "-18 > -19" tells you nothing about whether to fix the code or the
        // bound.
        let text = case(&result("clean", false), false);
        assert!(text.contains("FAIL outputLufs = -18.00 (max -19)"));
        assert!(text.contains("the reason this bound is the right one"));
    }

    #[test]
    fn a_skipped_case_says_so_rather_than_looking_like_a_pass() {
        let mut skipped = result("clean-denoise-onnx", true);
        skipped.skipped = Some("no model weights".into());
        let text = case(&skipped, false);
        assert!(text.contains("skipped: no model weights"));
        assert!(!text.contains('✓'));
    }

    #[test]
    fn infinities_are_spelled_rather_than_formatted() {
        let text = case(&result("bypass-null", true), true);
        assert!(text.contains("changeDb                 -inf"));
    }

    #[test]
    fn the_summary_counts_skips_separately_from_passes() {
        let mut skipped = result("onnx", true);
        skipped.skipped = Some("no weights".into());
        let results = [result("a", true), result("b", false), skipped];
        let text = summary(&results, 1, 1);
        assert!(text.contains("1/2 cases passed"), "{text}");
        assert!(text.contains("1 skipped"), "{text}");
    }

    #[test]
    fn an_unchanged_run_says_nothing_moved() {
        let results = [result("clean", true)];
        let baseline: Baseline = [(
            "clean".to_string(),
            [
                ("inputLufs".to_string(), -30.0),
                ("outputLufs".to_string(), -18.0),
                ("changeDb".to_string(), f64::NEG_INFINITY),
            ]
            .into_iter()
            .collect(),
        )]
        .into_iter()
        .collect();
        assert!(baseline_diff(&results, &baseline).contains("nothing moved"));
    }

    #[test]
    fn a_moved_metric_is_reported_with_its_delta() {
        let results = [result("clean", true)];
        let baseline: Baseline = [(
            "clean".to_string(),
            [("outputLufs".to_string(), -19.5)].into_iter().collect(),
        )]
        .into_iter()
        .collect();
        let text = baseline_diff(&results, &baseline);
        assert!(text.contains("outputLufs -19.50 → -18.00 (+1.50)"), "{text}");
    }

    #[test]
    fn json_writes_null_where_a_value_is_infinite() {
        // JSON has no infinity, and every bypass case has one.
        let text = json(&[result("bypass-null", true)]);
        let parsed: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(parsed[0]["metrics"]["changeDb"], Value::Null);
        assert_eq!(parsed[0]["metrics"]["outputLufs"], json!(-18.0));
    }
}
