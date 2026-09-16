use std::env;
use std::future::Future;
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use tokio::sync::watch;

use crate::engine::EvaluationCoverage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub(crate) fn for_terminal(self, is_terminal: bool) -> bool {
        self.enabled(
            is_terminal,
            env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
            env::var_os("TERM").is_some_and(|value| value == "dumb"),
        )
    }

    fn enabled(self, is_terminal: bool, no_color: bool, dumb_terminal: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => is_terminal && !no_color && !dumb_terminal,
        }
    }
}

pub(crate) async fn with_progress<F>(
    future: F,
    progress: watch::Receiver<EvaluationCoverage>,
    display: &ProgressDisplay,
) -> F::Output
where
    F: Future,
{
    if !display.enabled {
        return future.await;
    }
    let mut ticker = tokio::time::interval(Duration::from_millis(120));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started = Instant::now();
    let mut frame = 0;
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            result = &mut future => { display.clear(); return result; },
            _ = ticker.tick() => {
                let coverage = progress.borrow();
                let completed = coverage.units_succeeded + coverage.units_failed + coverage.units_previewed;
                let total = if coverage.discovery_complete {
                    coverage.units_total.to_string()
                } else {
                    format!("{}+", coverage.units_total)
                };
                let text = format!(
                    "{} {completed}/{} passages | {} routed | {} cached | {} failed | {:.1}s",
                    ['|', '/', '-', '\\'][frame % 4],
                    total,
                    coverage.files_routed,
                    coverage.cache_hits + coverage.routing_cache_hits,
                    coverage.units_failed + coverage.routing_failed,
                    started.elapsed().as_secs_f64(),
                );
                // Status output is best-effort; losing stderr must not cancel evaluation.
                if let Ok(mut line) = display.line.lock() { let _ = line.update(&text, display.color); }
                frame = frame.wrapping_add(1);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProgressDisplay {
    enabled: bool,
    color: bool,
    line: Mutex<ProgressLine>,
}

impl ProgressDisplay {
    pub(crate) fn new(enabled: bool, color: bool) -> Self {
        Self {
            enabled,
            color,
            line: Mutex::new(ProgressLine::default()),
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut line) = self.line.lock() {
            line.clear();
        }
    }
}

#[derive(Debug, Default)]
struct ProgressLine {
    columns: usize,
}

impl ProgressLine {
    fn clear(&mut self) {
        if self.columns > 0 {
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "\r{:width$}\r", "", width = self.columns);
            let _ = stderr.flush();
            self.columns = 0;
        }
    }
    fn update(&mut self, text: &str, color: bool) -> io::Result<()> {
        let mut stderr = io::stderr().lock();
        let padding = self.columns.saturating_sub(text.len());
        self.columns = text.len();
        if color {
            write!(stderr, "\r\x1b[36m{text}\x1b[0m{:padding$}", "")?;
        } else {
            write!(stderr, "\r{text}{:padding$}", "")?;
        }
        stderr.flush()
    }
}

impl Drop for ProgressLine {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case(ColorMode::Auto, true, false, false, true; "interactive terminal")]
    #[test_case(ColorMode::Auto, false, false, false, false; "pipe")]
    #[test_case(ColorMode::Auto, true, true, false, false; "no color environment")]
    #[test_case(ColorMode::Auto, true, false, true, false; "dumb terminal")]
    #[test_case(ColorMode::Always, false, true, true, true; "explicit override")]
    #[test_case(ColorMode::Never, true, false, false, false; "explicit plain output")]
    fn color_policy(mode: ColorMode, tty: bool, no_color: bool, dumb: bool, expected: bool) {
        assert_eq!(mode.enabled(tty, no_color, dumb), expected);
    }
}
