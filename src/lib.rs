//! Re-run a benchmark claim in a clean working tree, aggregate the samples,
//! and decide PASS/FAIL against a threshold declared in a TOML spec.
//!
//! The core is intentionally small and dependency-light so the verdict can be
//! audited: parse a TOML claim, run the command `N` times, extract a numeric
//! metric from each run's stdout with a regex, compute the median and a
//! percentile bootstrap confidence interval, then compare to the threshold.

#![deny(missing_docs, rust_2018_idioms)]

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use thiserror::Error;

/// Errors produced while loading a claim or running a verification.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O failure (read spec, create temp dir, etc.).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The TOML claim could not be parsed.
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// The metric regex is invalid.
    #[error("regex: {0}")]
    Regex(#[from] regex::Error),
    /// A single run of the benchmark command failed.
    #[error("command failed (exit {status}): {stderr}")]
    CommandFailed {
        /// Process exit status code (or -1 if unavailable).
        status: i32,
        /// Captured stderr.
        stderr: String,
    },
    /// The metric regex did not match the command stdout.
    #[error("metric regex did not match stdout (pattern `{pattern}`)")]
    MetricNotFound {
        /// The regex pattern that failed to match.
        pattern: String,
    },
    /// The capture group did not parse as `f64`.
    #[error("metric value `{raw}` is not a finite f64")]
    BadMetric {
        /// The raw capture text.
        raw: String,
    },
    /// Not enough successful samples to form a verdict.
    #[error("need at least {need} samples, got {got}")]
    TooFewSamples {
        /// Required sample count.
        need: usize,
        /// Actual sample count.
        got: usize,
    },
    /// JSON serialization failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A benchmark claim declared in TOML.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Claim {
    /// Human-readable name of the claim.
    pub name: String,
    /// Shell command to run (executed via `sh -c`). Relative paths resolve
    /// against the working directory (`workdir` or the clone root).
    pub command: String,
    /// Regex with exactly one capture group that extracts the metric as `f64`.
    pub metric_regex: String,
    /// How many times to run `command`. Must be ≥ 1 (odd values make the
    /// median a real sample; even values average the two central samples).
    pub runs: usize,
    /// Comparison operator and threshold.
    pub threshold: Threshold,
    /// Optional working directory relative to the clone / project root.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Optional bootstrap confidence level in (0, 1). Default 0.95.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Optional number of bootstrap resamples. Default 2000.
    #[serde(default = "default_bootstrap")]
    pub bootstrap_samples: usize,
    /// Optional per-run timeout in seconds. `None` means no timeout.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn default_confidence() -> f64 {
    0.95
}
fn default_bootstrap() -> usize {
    2000
}

/// Threshold side of a claim.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Threshold {
    /// Median must be ≥ value.
    Min(f64),
    /// Median must be ≤ value.
    Max(f64),
    /// Median must lie inside `[lo, hi]` (inclusive).
    Range {
        /// Lower bound.
        lo: f64,
        /// Upper bound.
        hi: f64,
    },
}

/// Verdict of one verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    /// Claim holds under the measured samples.
    Pass,
    /// Claim does not hold.
    Fail,
}

/// Full machine-readable report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// Claim name.
    pub name: String,
    /// PASS or FAIL.
    pub verdict: Verdict,
    /// Measured samples (successful runs only), in run order.
    pub samples: Vec<f64>,
    /// Median of `samples`.
    pub median: f64,
    /// Bootstrap percentile CI lower bound.
    pub ci_low: f64,
    /// Bootstrap percentile CI upper bound.
    pub ci_high: f64,
    /// Confidence level used for the CI.
    pub confidence: f64,
    /// Threshold as restated text.
    pub threshold: String,
    /// One-line human reason.
    pub reason: String,
}

impl Claim {
    /// Load a claim from a TOML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        let claim: Claim = toml::from_str(&text)?;
        if claim.runs == 0 {
            return Err(Error::TooFewSamples { need: 1, got: 0 });
        }
        if !(claim.confidence > 0.0 && claim.confidence < 1.0) {
            return Err(Error::BadMetric {
                raw: format!("confidence={}", claim.confidence),
            });
        }
        // Validate regex early.
        let _ = Regex::new(&claim.metric_regex)?;
        Ok(claim)
    }
}

/// Compute the median of a non-empty slice. Panics if empty — callers must
/// check length first.
pub fn median(xs: &[f64]) -> f64 {
    assert!(!xs.is_empty());
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Percentile bootstrap CI for the median.
///
/// Draws `B` resamples of size `n` with replacement (deterministic LCG so
/// runs are reproducible given the same samples), computes the median of
/// each, and returns the empirical `α/2` and `1-α/2` percentiles.
pub fn bootstrap_median_ci(samples: &[f64], confidence: f64, b: usize) -> (f64, f64) {
    assert!(!samples.is_empty());
    assert!(b >= 1);
    let n = samples.len();
    let alpha = 1.0 - confidence;
    let mut medians = Vec::with_capacity(b);
    // Simple LCG — good enough for a bootstrap, no external RNG crate.
    let mut state: u64 = 0xC0FFEE_u64
        .wrapping_mul(n as u64)
        .wrapping_add(samples.len() as u64 * 0x9E37);
    for _ in 0..b {
        let mut resample = Vec::with_capacity(n);
        for _ in 0..n {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let idx = (state >> 33) as usize % n;
            resample.push(samples[idx]);
        }
        medians.push(median(&resample));
    }
    medians.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo_idx = ((alpha / 2.0) * (b as f64)).floor() as usize;
    let hi_idx = (((1.0 - alpha / 2.0) * (b as f64)).ceil() as usize).min(b - 1);
    (medians[lo_idx], medians[hi_idx])
}

fn threshold_holds(median: f64, t: &Threshold) -> bool {
    match *t {
        Threshold::Min(v) => median >= v,
        Threshold::Max(v) => median <= v,
        Threshold::Range { lo, hi } => median >= lo && median <= hi,
    }
}

fn threshold_text(t: &Threshold) -> String {
    match *t {
        Threshold::Min(v) => format!("median >= {v}"),
        Threshold::Max(v) => format!("median <= {v}"),
        Threshold::Range { lo, hi } => format!("{lo} <= median <= {hi}"),
    }
}

/// Run `command` once in `workdir`, extract the metric with `re`.
pub fn run_once(
    command: &str,
    workdir: &Path,
    re: &Regex,
    timeout: Option<Duration>,
) -> Result<f64> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Manual timeout: we avoid the `wait-timeout` crate to keep deps small.
    let output = if let Some(limit) = timeout {
        let start = std::time::Instant::now();
        loop {
            match child.try_wait()? {
                Some(_) => break child.wait_with_output()?,
                None if start.elapsed() > limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::CommandFailed {
                        status: -1,
                        stderr: format!("timed out after {limit:?}"),
                    });
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    } else {
        child.wait_with_output()?
    };

    if !output.status.success() {
        return Err(Error::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let caps = re.captures(&stdout).ok_or_else(|| Error::MetricNotFound {
        pattern: re.as_str().to_string(),
    })?;
    let raw = caps
        .get(1)
        .map(|m| m.as_str())
        .ok_or_else(|| Error::MetricNotFound {
            pattern: re.as_str().to_string(),
        })?;
    let value: f64 = raw.parse().map_err(|_| Error::BadMetric {
        raw: raw.to_string(),
    })?;
    if !value.is_finite() {
        return Err(Error::BadMetric {
            raw: raw.to_string(),
        });
    }
    Ok(value)
}

/// Verify a claim against an already-checked-out project root.
pub fn verify(claim: &Claim, project_root: &Path) -> Result<Report> {
    let re = Regex::new(&claim.metric_regex)?;
    let workdir: PathBuf = match &claim.workdir {
        Some(rel) => project_root.join(rel),
        None => project_root.to_path_buf(),
    };
    let timeout = claim.timeout_secs.map(Duration::from_secs);

    let mut samples = Vec::with_capacity(claim.runs);
    for i in 0..claim.runs {
        match run_once(&claim.command, &workdir, &re, timeout) {
            Ok(v) => samples.push(v),
            Err(e) => {
                // Fail the whole claim on the first hard error — silent
                // retries would hide flaky benches.
                let _ = i;
                return Err(e);
            }
        }
    }
    if samples.is_empty() {
        return Err(Error::TooFewSamples { need: 1, got: 0 });
    }

    let med = median(&samples);
    let (ci_low, ci_high) =
        bootstrap_median_ci(&samples, claim.confidence, claim.bootstrap_samples);
    let holds = threshold_holds(med, &claim.threshold);
    let thr = threshold_text(&claim.threshold);
    let verdict = if holds { Verdict::Pass } else { Verdict::Fail };
    let reason = if holds {
        format!("median {med} satisfies {thr} (CI [{ci_low}, {ci_high}])")
    } else {
        format!("median {med} does not satisfy {thr} (CI [{ci_low}, {ci_high}])")
    };

    Ok(Report {
        name: claim.name.clone(),
        verdict,
        samples,
        median: med,
        ci_low,
        ci_high,
        confidence: claim.confidence,
        threshold: thr,
        reason,
    })
}

/// Clone `url` (optional `git_ref`) into a temporary directory and verify.
///
/// The temporary directory is returned alongside the report so the caller can
/// inspect the tree; it is **not** deleted automatically.
pub fn verify_in_clone(
    claim: &Claim,
    url: &str,
    git_ref: Option<&str>,
) -> Result<(Report, PathBuf)> {
    let tmp = {
        let base = std::env::temp_dir().join(format!(
            "bench-verify-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        base
    };

    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(r) = git_ref {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg(url).arg(&tmp);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(Error::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let report = verify(claim, &tmp)?;
    Ok((report, tmp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    #[test]
    fn bootstrap_ci_contains_median_for_constant() {
        let samples = vec![10.0; 9];
        let (lo, hi) = bootstrap_median_ci(&samples, 0.95, 500);
        assert!((lo - 10.0).abs() < 1e-9);
        assert!((hi - 10.0).abs() < 1e-9);
    }

    #[test]
    fn threshold_min_max_range() {
        assert!(threshold_holds(5.0, &Threshold::Min(5.0)));
        assert!(!threshold_holds(4.9, &Threshold::Min(5.0)));
        assert!(threshold_holds(1.0, &Threshold::Max(1.0)));
        assert!(threshold_holds(3.0, &Threshold::Range { lo: 1.0, hi: 5.0 }));
        assert!(!threshold_holds(
            0.5,
            &Threshold::Range { lo: 1.0, hi: 5.0 }
        ));
    }

    #[test]
    fn parse_claim_toml() {
        let toml = r#"
name = "demo"
command = "echo thrpt: 12.5 Melem/s"
metric_regex = 'thrpt:\\s*([0-9.]+)'
runs = 3
confidence = 0.95
bootstrap_samples = 100

[threshold]
min = 10.0
"#;
        let c: Claim = toml::from_str(toml).unwrap();
        assert_eq!(c.name, "demo");
        assert_eq!(c.threshold, Threshold::Min(10.0));
        assert_eq!(c.runs, 3);
    }
}
