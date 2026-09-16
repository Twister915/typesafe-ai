use std::io::{self, Write};

use serde::Serialize;
use typesafe_ai::Request;

use crate::engine::{EvaluationCoverage, EvaluationFailure, Match, Mode};
use crate::scanner::ScanIssue;

const SEPARATOR: &str = "────────────────────────────────────────────────────────────────────────";

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct SearchInfo<'a> {
    pub(crate) command: Mode,
    pub(crate) query: &'a str,
    pub(crate) model: &'a str,
    pub(crate) threshold: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Summary<'a> {
    #[serde(flatten)]
    pub(crate) search: SearchInfo<'a>,
    pub(crate) matched: usize,
    pub(crate) returned: usize,
    pub(crate) truncated: usize,
    pub(crate) coverage: &'a EvaluationCoverage,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RenderOptions {
    pub(crate) color: bool,
    pub(crate) preview_lines: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct Printer<W>
where
    W: Write,
{
    writer: W,
    json: bool,
    options: RenderOptions,
    index: usize,
}

impl<W> Printer<W>
where
    W: Write,
{
    pub(crate) fn new(writer: W, json: bool, options: RenderOptions) -> Self {
        Self {
            writer,
            json,
            options,
            index: 0,
        }
    }

    pub(crate) fn start(&mut self, info: &SearchInfo<'_>) -> io::Result<()> {
        if self.json {
            write_record(&mut self.writer, &JsonRecord::Start { info })?;
        } else {
            write_header(&mut self.writer, info, self.options)?;
        }
        self.writer.flush()
    }

    pub(crate) fn matched(&mut self, info: &SearchInfo<'_>, item: &Match) -> io::Result<()> {
        self.index += 1;
        if self.json {
            write_record(
                &mut self.writer,
                &JsonRecord::Match {
                    index: self.index,
                    item,
                },
            )?;
        } else {
            write_match(&mut self.writer, info, item, self.index, self.options)?;
        }
        self.writer.flush()
    }

    pub(crate) fn scan_issue(&mut self, issue: &ScanIssue) -> io::Result<()> {
        if self.json {
            write_record(&mut self.writer, &JsonRecord::ScanIssue { issue })?;
        } else {
            writeln!(
                self.writer,
                "{} {}: {}",
                paint(self.options, "1;31", "scan issue"),
                sanitize(&issue.path.display().to_string()),
                sanitize(&issue.message)
            )?;
        }
        self.writer.flush()
    }

    pub(crate) fn failure(&mut self, failure: &EvaluationFailure) -> io::Result<()> {
        if self.json {
            write_record(&mut self.writer, &JsonRecord::Failure { failure })?;
        } else {
            let path = sanitize(&failure.path.display().to_string());
            let location = location(&path, failure.start_line, failure.end_line);
            writeln!(
                self.writer,
                "{} {}: {}",
                paint(self.options, "1;31", "evaluation failure"),
                location,
                sanitize(&failure.message)
            )?;
        }
        self.writer.flush()
    }

    pub(crate) fn request(&mut self, request: &Request) -> io::Result<()> {
        if self.json {
            write_record(&mut self.writer, &JsonRecord::Request { request })?;
        } else {
            serde_json::to_writer(&mut self.writer, request)?;
            writeln!(self.writer)?;
        }
        self.writer.flush()
    }

    pub(crate) fn routed(&mut self, decision: &crate::routing::Decision) -> io::Result<()> {
        if self.json {
            write_record(&mut self.writer, &JsonRecord::Routing { decision })?;
        } else {
            writeln!(
                self.writer,
                "{} {} → {} · {} windows{}",
                paint(self.options, "1;36", "segmenter"),
                sanitize(&decision.path.display().to_string()),
                decision
                    .segmenter
                    .unwrap_or("pending model judgment (dry run)"),
                decision.windows,
                if decision.reason == "ambiguous_or_unsupported" {
                    " · generic fallback"
                } else {
                    ""
                },
            )?;
        }
        self.writer.flush()
    }

    pub(crate) fn finish(&mut self, summary: &Summary<'_>) -> io::Result<()> {
        if self.json {
            write_record(
                &mut self.writer,
                &JsonRecord::Summary {
                    summary,
                    complete: summary.coverage.complete(),
                },
            )?;
        } else {
            if summary.returned == 0 {
                write_empty_state(&mut self.writer, summary, self.options)?;
            }
            if summary.truncated > 0 {
                let notice = format!(
                    "Showing {} of {} threshold matches; --top omitted {}.",
                    summary.returned, summary.matched, summary.truncated
                );
                writeln!(self.writer, "{}", paint(self.options, "1;33", &notice))?;
                writeln!(self.writer)?;
            }
            write_summary(&mut self.writer, summary, self.options)?;
        }
        self.writer.flush()
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JsonRecord<'a> {
    Start {
        #[serde(flatten)]
        info: &'a SearchInfo<'a>,
    },
    Match {
        index: usize,
        #[serde(flatten)]
        item: &'a Match,
    },
    ScanIssue {
        #[serde(flatten)]
        issue: &'a ScanIssue,
    },
    Failure {
        #[serde(flatten)]
        failure: &'a EvaluationFailure,
    },
    Request {
        request: &'a Request,
    },
    Routing {
        #[serde(flatten)]
        decision: &'a crate::routing::Decision,
    },
    Summary {
        complete: bool,
        #[serde(flatten)]
        summary: &'a Summary<'a>,
    },
}

fn write_record(writer: &mut impl Write, record: &JsonRecord<'_>) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, record)?;
    writeln!(writer)
}

pub(crate) fn write_error(mut writer: impl Write, message: &str, color: bool) -> io::Result<()> {
    let options = RenderOptions {
        color,
        preview_lines: None,
    };
    writeln!(
        writer,
        "{} {}",
        paint(options, "1;31", "error:"),
        sanitize(message)
    )?;
    writer.flush()
}

fn write_header(
    writer: &mut impl Write,
    info: &SearchInfo<'_>,
    options: RenderOptions,
) -> io::Result<()> {
    writeln!(
        writer,
        "{}",
        paint(options, "1;36", &format!("tsg {}", command(info.command)))
    )?;
    writeln!(
        writer,
        "{} {}",
        paint(options, "1", "Query:"),
        sanitize(info.query)
    )?;
    writeln!(
        writer,
        "{} {}  {} {:.3}",
        paint(options, "1", "Model:"),
        sanitize(info.model),
        paint(options, "1", "Threshold:"),
        info.threshold
    )?;
    writeln!(writer, "{}", paint(options, "2", SEPARATOR))?;
    writeln!(writer)
}

fn write_match(
    writer: &mut impl Write,
    info: &SearchInfo<'_>,
    item: &Match,
    index: usize,
    options: RenderOptions,
) -> io::Result<()> {
    let path = sanitize(&item.unit.path.display().to_string());
    let location = location(&path, item.unit.start_line, item.unit.end_line);
    writeln!(
        writer,
        "{} {}",
        paint(options, "1", &format!("{index}.")),
        paint(options, "1;36", &location)
    )?;

    let probability_label = match info.command {
        Mode::Find => "relevance probability",
        Mode::Grep => "match probability",
    };
    let probability = probability_indicator(item.probability);
    let probability_style = if item.probability >= 0.8 {
        "1;32"
    } else if item.probability >= info.threshold {
        "1;33"
    } else {
        "1"
    };
    writeln!(
        writer,
        "   {}  {}",
        paint(options, "2", probability_label),
        paint(options, probability_style, &probability)
    )?;

    if !item.unit.headings.is_empty() {
        write!(writer, "   {} ", paint(options, "2", "section"))?;
        for (index, heading) in item.unit.headings.iter().enumerate() {
            if index > 0 {
                write!(writer, "{}", paint(options, "35", " › "))?;
            }
            write!(writer, "{}", paint(options, "35", &sanitize(heading)))?;
        }
        writeln!(writer)?;
    }
    writeln!(
        writer,
        "   {}",
        paint(
            options,
            "2",
            &format!(
                "{} · bytes {}..{}",
                sanitize(&item.unit.kind),
                item.unit.start_byte,
                item.unit.end_byte
            )
        )
    )?;
    writeln!(writer, "   {}", paint(options, "2", SEPARATOR))?;
    write_source(writer, item, options)?;
    writeln!(writer)
}

fn write_source(writer: &mut impl Write, item: &Match, options: RenderOptions) -> io::Result<()> {
    let total = item.unit.target.lines().count();
    let visible = options.preview_lines.unwrap_or(total).min(total);
    let line_number_width = item
        .unit
        .end_line
        .max(item.unit.start_line)
        .to_string()
        .len();
    for (offset, line) in item.unit.target.lines().take(visible).enumerate() {
        let line_number = item.unit.start_line.saturating_add(offset);
        writeln!(
            writer,
            "   {} {} {}",
            paint(
                options,
                "2;36",
                &format!("{line_number:>line_number_width$}")
            ),
            paint(options, "2", "│"),
            sanitize(line)
        )?;
    }

    let hidden = total.saturating_sub(visible);
    if hidden > 0 {
        let noun = if hidden == 1 { "line" } else { "lines" };
        let message = format!(
            "… {hidden} source {noun} hidden · through line {} · use --full to show all",
            item.unit.end_line
        );
        writeln!(writer, "   {}", paint(options, "1;33", &message))?;
    }
    Ok(())
}

fn write_empty_state(
    writer: &mut impl Write,
    summary: &Summary<'_>,
    options: RenderOptions,
) -> io::Result<()> {
    let coverage = summary.coverage;
    let headline = if coverage.routing_previewed > 0 {
        "Dry run: routing requests previewed; use --unit MODE to preview search passages."
    } else if coverage.units_previewed > 0 {
        "Dry run complete; requests were previewed, not scored."
    } else if coverage.units_total == 0 {
        "No searchable passages found."
    } else if coverage.complete() {
        "No passages met the threshold."
    } else {
        "No passages were returned from this incomplete scan."
    };
    writeln!(writer, "{}", paint(options, "1;33", headline))?;
    if coverage.units_previewed == 0 && coverage.routing_previewed == 0 {
        if coverage.units_total == 0 {
            writeln!(
                writer,
                "Check input paths, ignore rules, and file-size limits."
            )?;
        } else if coverage.units_succeeded > 0 {
            writeln!(
                writer,
                "Try lowering --threshold (or use --threshold 0 to inspect all ranked passages)."
            )?;
        }
    }
    if !coverage.complete() {
        writeln!(
            writer,
            "Review the streamed problems above, then rerun the scan."
        )?;
    }
    writeln!(writer)
}

fn write_summary(
    writer: &mut impl Write,
    summary: &Summary<'_>,
    options: RenderOptions,
) -> io::Result<()> {
    let coverage = summary.coverage;
    let (mark, state, style) = if coverage.complete() {
        ("✓", "Complete", "1;32")
    } else {
        ("!", "Incomplete", "1;31")
    };
    if coverage.units_previewed == 0 && coverage.routing_previewed == 0 {
        writeln!(
            writer,
            "{} · {}/{} units scored · {}/{} files · {} scheduled · {} cached",
            paint(options, style, &format!("{mark} {state}")),
            coverage.units_succeeded,
            coverage.units_total,
            coverage.files_scanned,
            coverage.files_discovered,
            coverage.evaluations_scheduled,
            coverage.cache_hits
        )?;
    } else {
        writeln!(
            writer,
            "{} · {} scored · {} previewed · {} total units · {}/{} files",
            paint(options, style, &format!("{mark} {state}")),
            coverage.units_succeeded,
            coverage.units_previewed,
            coverage.units_total,
            coverage.files_scanned,
            coverage.files_discovered
        )?;
    }
    if coverage.routing_windows > 0 {
        writeln!(
            writer,
            "  {} files routed · {}/{} routing windows scored · {} previewed · {} cached · {} failed · {} cancelled",
            coverage.files_routed,
            coverage.routing_succeeded,
            coverage.routing_windows,
            coverage.routing_previewed,
            coverage.routing_cache_hits,
            coverage.routing_failed,
            coverage.routing_cancelled
        )?;
    }
    if !coverage.complete() {
        writeln!(
            writer,
            "  {} evaluation failures · {} cancelled · {} scan issues · discovery {}",
            coverage.units_failed,
            coverage.units_cancelled,
            coverage.scan_issues,
            if coverage.discovery_complete {
                "complete"
            } else {
                "incomplete"
            }
        )?;
    }
    Ok(())
}

fn command(mode: Mode) -> &'static str {
    match mode {
        Mode::Find => "find",
        Mode::Grep => "grep",
    }
}

fn location(path: &str, start_line: usize, end_line: usize) -> String {
    if start_line == end_line {
        format!("{path}:{start_line}")
    } else {
        format!("{path}:{start_line}-{end_line}")
    }
}

fn probability_indicator(probability: f64) -> String {
    const WIDTH: usize = 20;
    let filled = (probability.clamp(0.0, 1.0) * WIDTH as f64).round() as usize;
    format!(
        "{probability:.3}  [{}{}]",
        "█".repeat(filled),
        "░".repeat(WIDTH - filled)
    )
}

fn paint(options: RenderOptions, code: &str, text: &str) -> String {
    if options.color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn sanitize(value: &str) -> String {
    let mut safe = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' => safe.push_str("\\n"),
            '\r' => safe.push_str("\\r"),
            '\t' => safe.push_str("\\t"),
            '\u{1b}' => safe.push_str("\\x1b"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                write!(safe, "\\u{{{:04X}}}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character => safe.push(character),
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;
    use crate::scanner::Unit;

    fn search() -> SearchInfo<'static> {
        SearchInfo {
            command: Mode::Find,
            query: "food truck requirements",
            model: "jev-latest",
            threshold: 0.5,
        }
    }

    fn complete_coverage() -> EvaluationCoverage {
        EvaluationCoverage {
            discovery_complete: true,
            files_discovered: 1,
            files_scanned: 1,
            units_total: 1,
            units_succeeded: 1,
            evaluations_scheduled: 1,
            ..EvaluationCoverage::default()
        }
    }

    fn sample_match() -> Match {
        Match {
            unit: Unit {
                path: PathBuf::from("laws/demo.md"),
                start_byte: 10,
                end_byte: 42,
                start_line: 7,
                end_line: 9,
                kind: "Markdown section".to_owned(),
                headings: vec!["Food Code".to_owned(), "Permits".to_owned()],
                target: "first line\nsecond line\nthird line".to_owned(),
                context: String::new(),
            },
            probability: 0.875,
        }
    }

    fn render(
        item: Option<&Match>,
        coverage: &EvaluationCoverage,
        color: bool,
        preview_lines: Option<usize>,
    ) -> String {
        let mut output = Vec::new();
        let mut printer = Printer::new(
            &mut output,
            false,
            RenderOptions {
                color,
                preview_lines,
            },
        );
        let info = search();
        printer.start(&info).unwrap();
        if let Some(item) = item {
            printer.matched(&info, item).unwrap();
        }
        printer
            .finish(&Summary {
                search: info,
                matched: usize::from(item.is_some()),
                returned: usize::from(item.is_some()),
                truncated: 0,
                coverage,
            })
            .unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn plain_renderer_has_no_ansi_and_exact_source_line_labels() {
        let output = render(Some(&sample_match()), &complete_coverage(), false, None);
        assert!(!output.contains("\x1b["));
        assert!(output.contains("relevance probability  0.875"));
        assert!(output.contains("Food Code › Permits"));
        assert!(output.contains(" 7 │ first line"));
        assert!(output.contains(" 8 │ second line"));
        assert!(output.contains(" 9 │ third line"));
        assert!(output.contains("✓ Complete"));
    }

    #[test]
    fn color_renderer_emits_ansi_styles() {
        let output = render(Some(&sample_match()), &complete_coverage(), true, None);
        assert!(output.contains("\x1b[1;36m"));
        assert!(output.contains("\x1b[1;32m"));
        assert!(output.contains("\x1b[0m"));
    }

    #[test]
    fn renderer_escapes_terminal_controls_in_streamed_fields() {
        let mut item = sample_match();
        item.unit.path = PathBuf::from("evil\u{1b}]8;;link\u{7}.md");
        item.unit.kind = "kind\rname".to_owned();
        item.unit.headings = vec!["head\tname".to_owned()];
        item.unit.target = "source\u{1b}[2J\u{85}line".to_owned();
        item.unit.end_line = item.unit.start_line;

        let mut output = Vec::new();
        let mut printer = Printer::new(&mut output, false, RenderOptions::default());
        let info = SearchInfo {
            query: "query\u{1b}[31m",
            model: "model\u{7}",
            ..search()
        };
        printer.start(&info).unwrap();
        printer.matched(&info, &item).unwrap();
        printer
            .failure(&EvaluationFailure {
                path: PathBuf::from("bad\u{1b}.md"),
                start_line: 1,
                end_line: 1,
                message: "failed\nagain".to_owned(),
            })
            .unwrap();
        printer
            .scan_issue(&ScanIssue {
                path: PathBuf::from("scan\u{7}.md"),
                message: "issue\ttext".to_owned(),
            })
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\u{7}'));
        assert!(!output.contains('\u{85}'));
        assert!(output.contains("query\\x1b[31m"));
        assert!(output.contains("head\\tname"));
        assert!(output.contains("source\\x1b[2J\\u{0085}line"));
        assert!(output.contains("failed\\nagain"));
    }

    #[test]
    fn preview_reports_exact_hidden_count_and_full_end_location() {
        let output = render(Some(&sample_match()), &complete_coverage(), false, Some(1));
        assert!(output.contains(" 7 │ first line"));
        assert!(!output.contains(" 8 │ second line"));
        assert!(output.contains("laws/demo.md:7-9"));
        assert!(
            output.contains("… 2 source lines hidden · through line 9 · use --full to show all")
        );
    }

    #[test]
    fn windows_line_endings_render_without_visible_carriage_returns() {
        let mut item = sample_match();
        item.unit.target = "first line\r\nsecond line\r\nthird line".to_owned();
        let output = render(Some(&item), &complete_coverage(), false, None);
        assert!(output.contains(" 7 │ first line\n"));
        assert!(output.contains(" 8 │ second line\n"));
        assert!(!output.contains("\\r"));
    }

    #[test]
    fn fatal_error_renderer_sanitizes_and_can_use_color() {
        let mut output = Vec::new();
        write_error(&mut output, "bad\u{1b}[2J\nmessage", true).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("\x1b[1;31merror:\x1b[0m "));
        assert!(output.contains("bad\\x1b[2J\\nmessage"));
        assert_eq!(output.matches('\u{1b}').count(), 2);
    }

    #[test]
    fn empty_partial_output_is_actionable_after_streamed_problem() {
        let coverage = EvaluationCoverage {
            discovery_complete: true,
            files_discovered: 1,
            files_scanned: 1,
            scan_issues: 1,
            units_total: 3,
            units_failed: 1,
            units_cancelled: 2,
            evaluations_scheduled: 1,
            ..EvaluationCoverage::default()
        };
        let output = render(None, &coverage, false, None);
        assert!(output.contains("No passages were returned from this incomplete scan."));
        assert!(!output.contains("Try lowering --threshold"));
        assert!(output.contains("! Incomplete"));
        assert!(output.contains("1 evaluation failures · 2 cancelled · 1 scan issues"));
        assert!(output.contains("streamed problems above"));
    }

    #[test]
    fn dry_run_summary_does_not_claim_requests_were_scored() {
        let coverage = EvaluationCoverage {
            discovery_complete: true,
            files_discovered: 1,
            files_scanned: 1,
            units_total: 2,
            units_previewed: 2,
            ..EvaluationCoverage::default()
        };
        let output = render(None, &coverage, false, None);
        assert!(output.contains("requests were previewed, not scored"));
        assert!(output.contains("0 scored · 2 previewed · 2 total units"));
        assert!(!output.contains("No passages met the threshold"));
        assert!(!output.contains("Try lowering --threshold"));
    }

    #[test]
    fn json_output_is_one_record_per_line_without_result_arrays() {
        let mut output = Vec::new();
        let mut printer = Printer::new(&mut output, true, RenderOptions::default());
        let info = search();
        let item = sample_match();
        let coverage = complete_coverage();
        printer.start(&info).unwrap();
        printer.matched(&info, &item).unwrap();
        printer
            .scan_issue(&ScanIssue {
                path: PathBuf::from("bad.md"),
                message: "bad input".to_owned(),
            })
            .unwrap();
        printer
            .failure(&EvaluationFailure {
                path: PathBuf::from("failed.md"),
                start_line: 3,
                end_line: 4,
                message: "no answer".to_owned(),
            })
            .unwrap();
        printer.request(&Request::new("state")).unwrap();
        printer
            .finish(&Summary {
                search: info,
                matched: 1,
                returned: 1,
                truncated: 0,
                coverage: &coverage,
            })
            .unwrap();

        let records = String::from_utf8(output).unwrap();
        let mut lines = records.lines();
        let start: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let matched: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let scan_issue: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let failure: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let request: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let summary: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(start["type"], "start");
        assert_eq!(matched["type"], "match");
        assert_eq!(matched["index"], 1);
        assert_eq!(scan_issue["type"], "scan_issue");
        assert_eq!(failure["type"], "failure");
        assert_eq!(request["type"], "request");
        assert_eq!(summary["type"], "summary");
        assert!(lines.next().is_none());
        assert!(summary.get("matches").is_none());
        assert!(summary.get("failures").is_none());
        assert!(summary.get("scan_issues").is_none());
    }

    #[derive(Debug, Default)]
    struct FlushCounter {
        output: Vec<u8>,
        flushes: usize,
    }

    impl Write for FlushCounter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn every_public_event_flushes_the_writer() {
        let mut writer = FlushCounter::default();
        let info = search();
        let coverage = complete_coverage();
        {
            let mut printer = Printer::new(&mut writer, true, RenderOptions::default());
            printer.start(&info).unwrap();
            printer.matched(&info, &sample_match()).unwrap();
            printer
                .scan_issue(&ScanIssue {
                    path: PathBuf::from("bad.md"),
                    message: "bad".to_owned(),
                })
                .unwrap();
            printer
                .failure(&EvaluationFailure {
                    path: PathBuf::from("bad.md"),
                    start_line: 1,
                    end_line: 1,
                    message: "bad".to_owned(),
                })
                .unwrap();
            printer.request(&Request::new("state")).unwrap();
            printer
                .finish(&Summary {
                    search: info,
                    matched: 1,
                    returned: 1,
                    truncated: 0,
                    coverage: &coverage,
                })
                .unwrap();
        }
        assert_eq!(writer.flushes, 6);
    }
}
