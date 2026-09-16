//! Search local source text by meaning with independent TypeSafe judgments.

mod cache;
mod engine;
mod output;
mod ranking;
mod routing;
mod scanner;
mod segmenters;
mod terminal;

use std::env;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use cache::Cache;
use clap::{Args, Parser, Subcommand};
use engine::{ApiEvaluator, Cancellation, EvaluationConfig, Mode, PipelineEvent, pipeline};
use futures_util::StreamExt;
use output::{Printer, RenderOptions, SearchInfo, Summary};
use ranking::TopMatches;
use scanner::{ScanConfig, Scanner, UnitMode};
use thiserror::Error;
use typesafe_ai::{ClientConfig, ReqwestClient};

#[cfg(test)]
mod tests;

#[derive(Debug, Parser)]
#[command(name = "tsg")]
#[command(about = "Search source files with independent TypeSafe semantic judgments")]
#[command(
    after_help = "Set TYPESAFE_API_KEY to evaluate. Use --dry-run to inspect requests without a key or network."
)]
struct Cli {
    /// When to use terminal colors (auto respects NO_COLOR and TERM=dumb).
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: terminal::ColorMode,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Rank passages relevant to a question, topic, or task.
    Find(SearchArgs),
    /// Return every passage independently judged to satisfy a condition.
    Grep(SearchArgs),
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// Natural-language question, topic, task, or semantic condition.
    query: String,

    /// Files or directories to scan.
    #[arg(default_value = ".", value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// TypeSafe model identifier.
    #[arg(long, default_value = "jev-latest")]
    model: String,

    /// Minimum Noul probability to return (find: 0.5, grep: 0.7).
    #[arg(long, value_parser = parse_probability)]
    threshold: Option<f64>,

    /// Results to retain for find (default: 20), or emit for grep (default: unlimited).
    #[arg(long, value_parser = parse_positive_usize)]
    top: Option<usize>,

    /// Shared concurrency limit for reading, routing, cache access, and evaluation.
    #[arg(long, default_value_t = 64, value_parser = parse_positive_usize)]
    concurrency: usize,

    /// Cache directory with one small probability entry per request; loaded on demand.
    #[arg(long, value_name = "PATH")]
    cache: Option<PathBuf>,

    /// Print the requests without requiring an API key or contacting TypeSafe.
    #[arg(long)]
    dry_run: bool,

    /// Emit newline-delimited JSON events as work completes, followed by a summary.
    #[arg(long)]
    json: bool,

    /// Maximum source lines shown per result; JSON always includes the full passage.
    #[arg(long, default_value_t = 12, value_parser = parse_positive_usize)]
    preview_lines: usize,

    /// Show the entire source passage for every result.
    #[arg(long, conflicts_with = "preview_lines")]
    full: bool,

    /// Hide the live terminal evaluation status.
    #[arg(long)]
    no_progress: bool,

    /// Maximum bytes read from one file.
    #[arg(long, default_value_t = 2 * 1024 * 1024, value_parser = parse_positive_u64)]
    max_file_bytes: u64,

    /// Maximum UTF-8 bytes in one target passage.
    #[arg(long, default_value_t = 24 * 1024, value_parser = parse_unit_bytes)]
    max_unit_bytes: usize,

    /// Maximum lines in a fallback text window.
    #[arg(long, default_value_t = 80, value_parser = parse_positive_usize)]
    window_lines: usize,

    /// Lines shared by adjacent fallback windows.
    #[arg(long, default_value_t = 20)]
    overlap_lines: usize,

    /// Segmenter (auto uses content-based Noul routing over the whole file).
    #[arg(long, value_enum, default_value = "auto")]
    unit: UnitMode,

    /// TypeSafe API base URL.
    #[arg(long, default_value = "https://api.typesafe.ai")]
    base_url: String,

    /// Timeout in seconds for each HTTP attempt.
    #[arg(long, default_value_t = 60, value_parser = parse_positive_u64)]
    timeout_seconds: u64,

    /// Retries after the initial request for HTTP 429 and 529 only.
    #[arg(long, default_value_t = 2)]
    retries: u32,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("--overlap-lines must be less than --window-lines")]
    WindowOverlap,
    #[error("TYPESAFE_API_KEY must contain a nonempty UTF-8 API key")]
    ApiKey,
    #[error("cannot configure TypeSafe client: {0}")]
    Client(#[from] typesafe_ai::ReqwestError),
    #[error("cache error: {0}")]
    Cache(String),
    #[error("cannot serialize dry-run requests: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot write output: {0}")]
    Output(#[from] io::Error),
}

fn parse_probability(value: &str) -> Result<f64, String> {
    let probability = value
        .parse::<f64>()
        .map_err(|_| "expected a number between 0 and 1".to_owned())?;
    if probability.is_finite() && (0.0..=1.0).contains(&probability) {
        Ok(probability)
    } else {
        Err("expected a finite number between 0 and 1".to_owned())
    }
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "expected a positive integer".to_owned())
}

fn parse_positive_u64(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "expected a positive integer".to_owned())
}

fn parse_unit_bytes(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value >= 16)
        .ok_or_else(|| "expected an integer of at least 16".to_owned())
}

#[derive(Debug, Clone, Copy)]
struct RunStatus {
    complete: bool,
    matched: Option<bool>,
}

async fn run(command: Command, color: terminal::ColorMode) -> Result<RunStatus, CliError> {
    let (mode, args) = match command {
        Command::Find(args) => (Mode::Find, args),
        Command::Grep(args) => (Mode::Grep, args),
    };
    if args.overlap_lines >= args.window_lines {
        return Err(CliError::WindowOverlap);
    }
    let threshold = args.threshold.unwrap_or(match mode {
        Mode::Find => 0.5,
        Mode::Grep => 0.7,
    });
    let render_options = RenderOptions {
        color: color.for_terminal(io::stdout().is_terminal()),
        preview_lines: (!args.full).then_some(args.preview_lines),
    };
    let cache = Cache::new(args.cache.clone()).map_err(CliError::Cache)?;
    let scanner = Scanner::new(
        &args.paths,
        ScanConfig {
            max_file_bytes: args.max_file_bytes,
            max_unit_bytes: args.max_unit_bytes,
            window_lines: args.window_lines,
            overlap_lines: args.overlap_lines,
            unit_mode: args.unit,
        },
        args.cache.as_deref(),
    );
    let evaluator = if args.dry_run {
        None
    } else {
        let api_key = env::var("TYPESAFE_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or(CliError::ApiKey)?;
        Some(ApiEvaluator::new(ReqwestClient::with_config(
            api_key,
            ClientConfig {
                base_url: args.base_url.clone(),
                timeout: Duration::from_secs(args.timeout_seconds),
                max_retries: args.retries,
            },
        )?))
    };
    let mut ranking = if mode == Mode::Find && !args.dry_run {
        Some(TopMatches::new(args.top.unwrap_or(20))?)
    } else {
        None
    };
    let info = SearchInfo {
        command: mode,
        query: &args.query,
        model: &args.model,
        threshold,
    };
    let mut printer = Printer::new(io::stdout().lock(), args.json, render_options);
    printer.start(&info)?;
    let cancellation = Cancellation::default();
    let signal_cancellation = cancellation.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });
    let show_progress = !args.json
        && !args.no_progress
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
        && env::var_os("TERM").is_none_or(|term| term != "dumb");
    let display = terminal::ProgressDisplay::new(
        show_progress,
        color.for_terminal(io::stderr().is_terminal()),
    );
    let (progress_tx, progress_rx) = tokio::sync::watch::channel(Default::default());
    let work = async {
        let events = pipeline(
            evaluator.as_ref(),
            scanner,
            EvaluationConfig {
                mode,
                query: &args.query,
                model: &args.model,
                concurrency: args.concurrency,
                cache_namespace: &args.base_url,
                dry_run: args.dry_run,
                progress: show_progress.then_some(&progress_tx),
            },
            &cache,
            cancellation,
        );
        let mut events = std::pin::pin!(events);
        let mut matched = 0;
        let mut returned = 0;
        let mut final_coverage = None;
        while let Some(event) = events.next().await {
            display.clear();
            match event {
                PipelineEvent::Match(item) if item.probability >= threshold => {
                    matched += 1;
                    if let Some(ranking) = &mut ranking {
                        ranking.consider(&item)?;
                    } else if args.top.is_none_or(|top| returned < top) {
                        printer.matched(&info, &item)?;
                        returned += 1;
                    }
                }
                PipelineEvent::Match(_) => {}
                PipelineEvent::Failure(failure) => printer.failure(&failure)?,
                PipelineEvent::ScanIssue(issue) => printer.scan_issue(&issue)?,
                PipelineEvent::Request(request) => printer.request(&request)?,
                PipelineEvent::Routed(decision) => printer.routed(&decision)?,
                PipelineEvent::Finished(coverage) => final_coverage = Some(coverage),
            }
        }
        if let Some(ranking) = &mut ranking {
            for item in ranking.drain_ranked() {
                printer.matched(&info, &item?)?;
                returned += 1;
            }
        }
        let coverage =
            final_coverage.ok_or_else(|| io::Error::other("pipeline ended without a summary"))?;
        printer.finish(&Summary {
            search: info,
            matched,
            returned,
            truncated: matched.saturating_sub(returned),
            coverage: &coverage,
        })?;
        Ok::<_, CliError>(RunStatus {
            complete: coverage.complete(),
            matched: (!args.dry_run).then_some(matched > 0),
        })
    };
    let result = terminal::with_progress(work, progress_rx, &display).await;
    signal_task.abort();
    result
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .init();
    let cli = Cli::parse();
    match run(cli.command, cli.color).await {
        Ok(RunStatus {
            complete: true,
            matched: Some(false),
        }) => ExitCode::from(1),
        Ok(RunStatus { complete: true, .. }) => ExitCode::SUCCESS,
        Ok(RunStatus {
            complete: false, ..
        }) => ExitCode::from(2),
        Err(error) => {
            let _ = output::write_error(
                io::stderr().lock(),
                &error.to_string(),
                cli.color.for_terminal(io::stderr().is_terminal()),
            );
            ExitCode::FAILURE
        }
    }
}
