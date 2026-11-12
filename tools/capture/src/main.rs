//! Command line for `llmobs-capture`.
//!
//! `CARGO_MANIFEST_DIR` is `tools/capture` when this binary is built with
//! `cargo run`. The tool is only run that way. User-facing lines go through
//! `tracing` so the workspace print lints hold.

use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::{Parser, Subcommand};
use llmobs_capture::{CaptureError, CasePath, Language, Program, discover, run_case};

fn main() -> ExitCode {
    init_tracing();
    match runtime() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn runtime() -> Result<(), CaptureError> {
    match Cli::parse().command {
        Command::List => list(),
        Command::Run { check, all, cases } => run(check, all, &cases).await,
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_level(false)
        .without_time()
        .init();
}

/// Record OTLP fixtures from real instrumentations.
#[derive(Debug, Parser)]
#[command(name = "llmobs-capture")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print discovered scenarios, one `group/case` per line.
    List,
    /// Run scenarios one at a time.
    Run {
        /// Compare with the committed fixtures instead of writing them.
        #[arg(long)]
        check: bool,
        /// Run every discovered scenario.
        #[arg(long, conflicts_with = "cases")]
        all: bool,
        /// Case paths (`group/case`).
        #[arg(required_unless_present = "all")]
        cases: Vec<String>,
    },
}

fn list() -> Result<(), CaptureError> {
    let root = repo_root()?;
    for (case, _, _) in discover(&root)? {
        tracing::info!("{case}");
    }
    Ok(())
}

async fn run(check: bool, all: bool, cases: &[String]) -> Result<(), CaptureError> {
    let root = repo_root()?;
    let discovered = discover(&root)?;
    let selected = select(&discovered, all, cases)?;
    if all && selected.is_empty() {
        tracing::info!("0 cases");
        return Ok(());
    }
    for (case, language, dir) in selected {
        let program = Program::for_language(&root, language);
        let outcome = run_case(&root, &dir, language, check, &program).await?;
        if check {
            tracing::info!("checked {case}");
        } else {
            tracing::info!("captured {case}");
            if outcome.stale_expected {
                tracing::info!("{case}: expected.json is stale; re-bless with LLMOBS_BLESS=1");
            }
        }
    }
    Ok(())
}

fn select(
    discovered: &[(CasePath, Language, PathBuf)],
    all: bool,
    cases: &[String],
) -> Result<Vec<(CasePath, Language, PathBuf)>, CaptureError> {
    if all {
        return Ok(discovered.to_vec());
    }
    let mut selected = Vec::with_capacity(cases.len());
    for text in cases {
        let path = CasePath::from_str(text)?;
        let Some(found) = discovered.iter().find(|(case, _, _)| case == &path) else {
            return Err(CaptureError::CaseNotFound { case: text.clone() });
        };
        selected.push(found.clone());
    }
    Ok(selected)
}

/// Repository root for a binary built from this crate.
///
/// `CARGO_MANIFEST_DIR` is `tools/capture` under `cargo run`, which is how this
/// dev tool is started. The path is canonical so the scrubber's repository
/// needle matches the scenario directory the program runs in.
fn repo_root() -> Result<PathBuf, CaptureError> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|source| CaptureError::Process {
            step: "find the repository root",
            source,
        })
}
