//! CLI entry point for `bench-verify`.

use bench_verify::{verify, verify_in_clone, Claim, Verdict};
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

/// Re-run a benchmark claim N times and PASS/FAIL against a TOML threshold.
#[derive(Debug, Parser)]
#[command(name = "bench-verify", version, about)]
struct Args {
    /// Path to the TOML claim file.
    #[arg(value_name = "CLAIM.toml")]
    claim: PathBuf,

    /// Project root where the command runs (default: current directory).
    /// Ignored when `--git` is set.
    #[arg(long, short = 'C', value_name = "DIR")]
    project: Option<PathBuf>,

    /// Clone this git URL into a temp dir and run there (clean tree).
    #[arg(long, value_name = "URL")]
    git: Option<String>,

    /// Branch or tag for `--git` (passed to `git clone --branch`).
    #[arg(long, value_name = "REF")]
    git_ref: Option<String>,

    /// Write the JSON report to this file (also always printed to stdout).
    #[arg(long, value_name = "FILE")]
    json_out: Option<PathBuf>,

    /// Print only the JSON report (no human summary on stderr).
    #[arg(long)]
    quiet: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let claim = match Claim::load(&args.claim) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bench-verify: failed to load claim: {e}");
            return ExitCode::from(2);
        }
    };

    if !args.quiet {
        eprintln!(
            "bench-verify: claim `{}` — {} run(s)",
            claim.name, claim.runs
        );
    }

    let report = if let Some(url) = args.git.as_deref() {
        if !args.quiet {
            eprintln!("bench-verify: cloning {url}");
        }
        match verify_in_clone(&claim, url, args.git_ref.as_deref()) {
            Ok((r, dir)) => {
                if !args.quiet {
                    eprintln!("bench-verify: clone at {}", dir.display());
                }
                r
            }
            Err(e) => {
                eprintln!("bench-verify: verification error: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        let root = args
            .project
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        if !args.quiet {
            eprintln!("bench-verify: project {}", root.display());
        }
        match verify(&claim, &root) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("bench-verify: verification error: {e}");
                return ExitCode::from(2);
            }
        }
    };

    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("bench-verify: json error: {e}");
            return ExitCode::from(2);
        }
    };
    println!("{json}");
    if let Some(path) = args.json_out {
        if let Err(e) = std::fs::write(&path, &json) {
            eprintln!("bench-verify: write {}: {e}", path.display());
            return ExitCode::from(2);
        }
    }
    if !args.quiet {
        let label = match report.verdict {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
        };
        eprintln!("bench-verify: {label} — {}", report.reason);
    }

    match report.verdict {
        Verdict::Pass => ExitCode::SUCCESS,
        Verdict::Fail => ExitCode::from(1),
    }
}
