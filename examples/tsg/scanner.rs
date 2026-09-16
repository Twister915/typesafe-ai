use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufReader, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clap::ValueEnum;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

const BUFFER_BYTES: usize = 8 * 1024;
const MAX_CONTEXT_BYTES: usize = 4 * 1024;
const MAX_HEADING_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum UnitMode {
    Auto,
    Javascript,
    Rust,
    Css,
    Prose,
    Section,
    Paragraph,
    Window,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanConfig {
    pub(crate) max_file_bytes: u64,
    pub(crate) max_unit_bytes: usize,
    pub(crate) window_lines: usize,
    pub(crate) overlap_lines: usize,
    pub(crate) unit_mode: UnitMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Unit {
    pub(crate) path: PathBuf,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) kind: String,
    pub(crate) headings: Vec<String>,
    pub(crate) target: String,
    pub(crate) context: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScanIssue {
    pub(crate) path: PathBuf,
    pub(crate) message: String,
}

#[derive(Debug)]
pub(crate) enum ScanEvent {
    FileStarted(PathBuf),
    Progress { path: PathBuf, bytes_scanned: u64 },
    Unit(Unit),
    RoutingChunk(Unit),
    RoutingReady(PathBuf),
    FileFinished(PathBuf),
    Issue(ScanIssue),
}

pub(crate) struct Scanner {
    walk: ignore::Walk,
    config: ScanConfig,
    current: Option<FileScanner<SnapshotReader>>,
    snapshot: Option<Snapshot>,
    routing: bool,
    parked: bool,
    pending: VecDeque<ScanEvent>,
}

impl Scanner {
    pub(crate) fn new(
        paths: &[PathBuf],
        config: ScanConfig,
        excluded_directory: Option<&Path>,
    ) -> Self {
        assert!(!paths.is_empty(), "Scanner requires at least one path");
        let mut builder = WalkBuilder::new(&paths[0]);
        for path in &paths[1..] {
            builder.add(path);
        }
        builder
            .standard_filters(true)
            .follow_links(false)
            .threads(1);
        if let Some(excluded) = excluded_directory {
            let excluded = absolute_lexical(excluded);
            let resolved = resolved_location(&excluded);
            builder.filter_entry(move |entry| {
                absolute_lexical(entry.path()) != excluded
                    && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                        || resolved_location(entry.path()) != resolved)
            });
        }
        Self {
            walk: builder.build(),
            config,
            current: None,
            snapshot: None,
            routing: false,
            parked: false,
            pending: VecDeque::new(),
        }
    }

    /// Discover one independent file scanner without opening or reading its contents.
    pub(crate) fn next_file(&mut self) -> Option<Result<Self, ScanIssue>> {
        self.next_path().map(|result| {
            result.map(|path| Self::new(std::slice::from_ref(&path), self.config, None))
        })
    }

    fn next_path(&mut self) -> Option<Result<PathBuf, ScanIssue>> {
        loop {
            match self.walk.next()? {
                Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                    return Some(Ok(entry.into_path()));
                }
                Ok(_) => {}
                Err(error) => {
                    return Some(Err(ScanIssue {
                        path: PathBuf::from("<walk>"),
                        message: error.to_string(),
                    }));
                }
            }
        }
    }

    fn open_file(&mut self, path: PathBuf) -> ScanEvent {
        self.pending.push_back(ScanEvent::FileStarted(path.clone()));
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.pending
                    .push_back(ScanEvent::Issue(io_issue(&path, error)));
                return self.pending.pop_front().unwrap();
            }
        };
        if metadata.len() > self.config.max_file_bytes {
            self.pending.push_back(ScanEvent::Issue(ScanIssue {
                path: path.clone(),
                message: format!(
                    "skipped: {} bytes exceeds --max-file-bytes {}",
                    metadata.len(),
                    self.config.max_file_bytes
                ),
            }));
            return self.pending.pop_front().unwrap();
        }
        match fs::File::open(&path) {
            Ok(file) => {
                let mut config = self.config;
                self.routing = config.unit_mode == UnitMode::Auto;
                let snapshot = if self.routing {
                    match Snapshot::new() {
                        Ok(snapshot) => Some(snapshot),
                        Err(error) => {
                            self.pending
                                .push_back(ScanEvent::Issue(io_issue(&path, error)));
                            return self.pending.pop_front().unwrap();
                        }
                    }
                } else {
                    None
                };
                let output = match snapshot
                    .as_ref()
                    .map(|s| s.file.as_ref().expect("open snapshot").try_clone())
                    .transpose()
                {
                    Ok(output) => output,
                    Err(error) => {
                        self.pending
                            .push_back(ScanEvent::Issue(io_issue(&path, error)));
                        return self.pending.pop_front().unwrap();
                    }
                };
                if self.routing {
                    config.unit_mode = UnitMode::Window;
                    config.overlap_lines = 0;
                    config.window_lines = usize::MAX;
                }
                self.snapshot = snapshot;
                self.current = Some(FileScanner::new(
                    SnapshotReader {
                        source: file,
                        output,
                        remaining: config.max_file_bytes,
                    },
                    path,
                    config,
                ));
            }
            Err(error) => self
                .pending
                .push_back(ScanEvent::Issue(io_issue(&path, error))),
        }
        self.pending.pop_front().unwrap()
    }

    pub(crate) fn select_mode(&mut self, mode: UnitMode) -> Result<(), ScanIssue> {
        let path = self
            .current
            .as_ref()
            .map(|file| file.path.clone())
            .unwrap_or_default();
        if !self.parked || mode == UnitMode::Auto {
            return Err(ScanIssue {
                path,
                message: "no parked file or unresolved segmenter".into(),
            });
        }
        let snapshot = self.snapshot.as_mut().expect("parked snapshot");
        snapshot
            .file
            .as_mut()
            .expect("open snapshot")
            .rewind()
            .map_err(|error| io_issue(&path, error))?;
        let source = snapshot
            .file
            .as_ref()
            .expect("open snapshot")
            .try_clone()
            .map_err(|error| io_issue(&path, error))?;
        let mut config = self.config;
        config.unit_mode = mode;
        self.current = Some(FileScanner::new(
            SnapshotReader {
                source,
                output: None,
                remaining: config.max_file_bytes,
            },
            path,
            config,
        ));
        self.routing = false;
        self.parked = false;
        Ok(())
    }

    pub(crate) fn skip_routing(&mut self) {
        self.current = None;
        self.snapshot = None;
        self.routing = false;
        self.parked = false;
    }
}

impl Iterator for Scanner {
    type Item = ScanEvent;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        if self.parked {
            return None;
        }
        if let Some(file) = &mut self.current {
            match file.next_event() {
                FileEvent::Unit(unit) => {
                    return Some(if self.routing {
                        ScanEvent::RoutingChunk(unit)
                    } else {
                        ScanEvent::Unit(unit)
                    });
                }
                FileEvent::Progress {
                    path,
                    bytes_scanned,
                } => {
                    return Some(ScanEvent::Progress {
                        path,
                        bytes_scanned,
                    });
                }
                FileEvent::Finished(path) => {
                    if self.routing {
                        self.parked = true;
                        return Some(ScanEvent::RoutingReady(path));
                    }
                    self.skip_routing();
                    return Some(ScanEvent::FileFinished(path));
                }
                FileEvent::Issue(issue) => {
                    self.skip_routing();
                    return Some(ScanEvent::Issue(issue));
                }
            }
        }

        Some(match self.next_path()? {
            Ok(path) => self.open_file(path),
            Err(issue) => ScanEvent::Issue(issue),
        })
    }
}

// Created with exclusive access and private permissions; deletion is tied to ownership.
struct Snapshot {
    file: Option<fs::File>,
    path: PathBuf,
}

impl Snapshot {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                ".tsg-snapshot-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(file),
                        path,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot create scan snapshot",
        ))
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

struct SnapshotReader {
    source: fs::File,
    output: Option<fs::File>,
    remaining: u64,
}

impl Read for SnapshotReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let allowance = self.remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let length = self.source.read(&mut buffer[..allowance])?;
        if let Some(output) = &mut self.output {
            let kept = length.min(self.remaining as usize);
            output.write_all(&buffer[..kept])?;
        }
        self.remaining = self.remaining.saturating_sub(length as u64);
        Ok(length)
    }
}

fn language_kind(mode: UnitMode) -> &'static str {
    match mode {
        UnitMode::Javascript => "JavaScript/TypeScript",
        UnitMode::Rust => "Rust",
        UnitMode::Css => "CSS",
        UnitMode::Prose => "Unicode sentence",
        _ => unreachable!("language mode"),
    }
}

fn absolute_lexical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn resolved_location(path: &Path) -> PathBuf {
    let absolute = absolute_lexical(path);
    for ancestor in absolute.ancestors() {
        if let Ok(resolved) = ancestor.canonicalize() {
            return resolved.join(absolute.strip_prefix(ancestor).expect("path ancestor"));
        }
    }
    absolute
}

fn io_issue(path: &Path, error: io::Error) -> ScanIssue {
    ScanIssue {
        path: path.to_owned(),
        message: error.to_string(),
    }
}

enum FileEvent {
    Unit(Unit),
    Progress { path: PathBuf, bytes_scanned: u64 },
    Finished(PathBuf),
    Issue(ScanIssue),
}

struct FileScanner<R>
where
    R: Read,
{
    path: PathBuf,
    input: StreamInput<R>,
    segmenter: Segmenter,
    terminal: Option<FileTerminal>,
}

enum FileTerminal {
    Finished,
    Issue(String),
}

impl<R> FileScanner<R>
where
    R: Read,
{
    fn new(reader: R, path: PathBuf, config: ScanConfig) -> Self {
        let mode = config.unit_mode;
        Self {
            path: path.clone(),
            input: StreamInput::new(reader, config.max_file_bytes),
            segmenter: Segmenter::new(path, config, mode),
            terminal: None,
        }
    }

    fn next_event(&mut self) -> FileEvent {
        let mut processed = 0;
        loop {
            if let Some(unit) = self.segmenter.take_ready() {
                return FileEvent::Unit(unit);
            }
            if let Some(terminal) = self.terminal.take() {
                return match terminal {
                    FileTerminal::Finished => FileEvent::Finished(self.path.clone()),
                    FileTerminal::Issue(message) => FileEvent::Issue(ScanIssue {
                        path: self.path.clone(),
                        message,
                    }),
                };
            }
            match self.input.next_item() {
                InputItem::Character { offset, character } => {
                    processed += character.len_utf8();
                    if character == '\0' {
                        self.segmenter.abort();
                        self.terminal = Some(FileTerminal::Issue(
                            "skipped: contains a NUL byte".to_owned(),
                        ));
                    } else {
                        self.segmenter.push(offset, character);
                    }
                    if self.segmenter.ready.is_empty() && processed >= BUFFER_BYTES {
                        return FileEvent::Progress {
                            path: self.path.clone(),
                            bytes_scanned: self.input.processed_offset as u64,
                        };
                    }
                }
                InputItem::End => {
                    self.segmenter.finish();
                    self.terminal = Some(FileTerminal::Finished);
                }
                InputItem::Issue(message) => {
                    self.segmenter.abort();
                    self.terminal = Some(FileTerminal::Issue(message));
                }
            }
        }
    }
}

enum InputItem {
    Character { offset: usize, character: char },
    End,
    Issue(String),
}

struct StreamInput<R>
where
    R: Read,
{
    reader: BufReader<R>,
    buffer: [u8; BUFFER_BYTES],
    input: String,
    input_position: usize,
    carry: Vec<u8>,
    bytes_read: u64,
    processed_offset: usize,
    max_bytes: u64,
    pending_issue: Option<String>,
    ended: bool,
}

impl<R> StreamInput<R>
where
    R: Read,
{
    fn new(reader: R, max_bytes: u64) -> Self {
        Self {
            reader: BufReader::with_capacity(BUFFER_BYTES, reader),
            buffer: [0; BUFFER_BYTES],
            input: String::new(),
            input_position: 0,
            carry: Vec::with_capacity(3),
            bytes_read: 0,
            processed_offset: 0,
            max_bytes,
            pending_issue: None,
            ended: false,
        }
    }

    fn next_item(&mut self) -> InputItem {
        loop {
            if self.input_position < self.input.len() {
                let character = self.input[self.input_position..].chars().next().unwrap();
                let offset = self.processed_offset;
                self.input_position += character.len_utf8();
                self.processed_offset += character.len_utf8();
                return InputItem::Character { offset, character };
            }
            if let Some(message) = self.pending_issue.take() {
                self.ended = true;
                return InputItem::Issue(message);
            }
            if self.ended {
                return InputItem::End;
            }
            if let Some(item) = self.refill() {
                return item;
            }
        }
    }

    fn refill(&mut self) -> Option<InputItem> {
        let allowance = self
            .max_bytes
            .saturating_sub(self.bytes_read)
            .saturating_add(1)
            .min(BUFFER_BYTES as u64) as usize;
        let read = match self.reader.read(&mut self.buffer[..allowance]) {
            Ok(read) => read,
            Err(error) => {
                self.ended = true;
                return Some(InputItem::Issue(format!("cannot read file: {error}")));
            }
        };
        if read == 0 {
            self.ended = true;
            if self.carry.is_empty() {
                return Some(InputItem::End);
            }
            self.carry.clear();
            return Some(InputItem::Issue(
                "skipped: input ended within a UTF-8 character".to_owned(),
            ));
        }
        self.bytes_read += read as u64;
        if self.bytes_read > self.max_bytes {
            self.ended = true;
            return Some(InputItem::Issue(format!(
                "skipped: file grew beyond --max-file-bytes {} while reading",
                self.max_bytes
            )));
        }

        let mut combined = std::mem::take(&mut self.carry);
        combined.extend_from_slice(&self.buffer[..read]);
        self.input_position = 0;
        match std::str::from_utf8(&combined) {
            Ok(_) => {
                self.input = String::from_utf8(combined)
                    .expect("combined input was validated immediately before conversion");
            }
            Err(error) => {
                let valid = error.valid_up_to();
                self.input = String::from_utf8(combined[..valid].to_vec())
                    .expect("the UTF-8 error reports a valid prefix");
                if error.error_len().is_some() {
                    self.pending_issue = Some(format!(
                        "skipped: invalid UTF-8 at byte {}",
                        self.processed_offset + valid
                    ));
                } else {
                    self.carry.extend_from_slice(&combined[valid..]);
                }
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedMode {
    Section,
    Paragraph,
    Window,
    Language(UnitMode),
}

struct Segmenter {
    path: PathBuf,
    config: ScanConfig,
    mode: ResolvedMode,
    line: usize,
    line_has_content: bool,
    builder: Option<UnitBuffer>,
    ready: VecDeque<Unit>,
    history: VecDeque<u8>,
    context_limit: usize,
    hierarchy: Vec<(usize, String)>,
    fence: Option<(char, usize)>,
    opening_fence: Option<char>,
    section_line: SectionLine,
    heading_raw: String,
    next_probe: usize,
    language_pending: bool,
    language_eof: bool,
}

struct UnitBuffer {
    start_byte: usize,
    start_line: usize,
    bytes: Vec<u8>,
    context: String,
}

enum SectionLine {
    Probe(Vec<CharToken>),
    Plain,
    Heading,
    Fenced(FenceTracker),
}

#[derive(Clone, Copy)]
struct CharToken {
    offset: usize,
    character: char,
}

enum ProbeDecision {
    More,
    Plain,
    Heading(usize),
    Fence(char, usize),
}

struct FenceTracker {
    marker: char,
    minimum: usize,
    phase: FencePhase,
}

enum FencePhase {
    Leading(usize),
    Markers(usize),
    Trailing,
    Invalid,
}

impl FenceTracker {
    fn new(marker: char, minimum: usize) -> Self {
        Self {
            marker,
            minimum,
            phase: FencePhase::Leading(0),
        }
    }

    fn push(&mut self, character: char) {
        self.phase = match self.phase {
            FencePhase::Leading(spaces) if character == ' ' && spaces < 3 => {
                FencePhase::Leading(spaces + 1)
            }
            FencePhase::Leading(_) if character == self.marker => FencePhase::Markers(1),
            FencePhase::Markers(count) if character == self.marker => {
                FencePhase::Markers(count + 1)
            }
            FencePhase::Markers(count) if count >= self.minimum && character.is_whitespace() => {
                FencePhase::Trailing
            }
            FencePhase::Trailing if character.is_whitespace() => FencePhase::Trailing,
            _ => FencePhase::Invalid,
        };
    }

    fn closes(&self) -> bool {
        matches!(self.phase, FencePhase::Markers(count) if count >= self.minimum)
            || matches!(self.phase, FencePhase::Trailing)
    }
}

impl Segmenter {
    fn new(path: PathBuf, config: ScanConfig, mode: UnitMode) -> Self {
        let mode = match mode {
            UnitMode::Section => ResolvedMode::Section,
            UnitMode::Paragraph => ResolvedMode::Paragraph,
            UnitMode::Window => ResolvedMode::Window,
            UnitMode::Javascript | UnitMode::Rust | UnitMode::Css | UnitMode::Prose => {
                ResolvedMode::Language(mode)
            }
            UnitMode::Auto => unreachable!("auto mode is resolved before segmentation"),
        };
        Self {
            path,
            config,
            mode,
            line: 1,
            line_has_content: false,
            builder: None,
            ready: VecDeque::with_capacity(2),
            history: VecDeque::with_capacity(
                config
                    .max_unit_bytes
                    .min(MAX_CONTEXT_BYTES)
                    .saturating_add(4),
            ),
            context_limit: config.max_unit_bytes.min(MAX_CONTEXT_BYTES),
            hierarchy: Vec::new(),
            fence: None,
            opening_fence: None,
            section_line: SectionLine::Probe(Vec::with_capacity(10)),
            heading_raw: String::with_capacity(MAX_HEADING_BYTES),
            next_probe: BUFFER_BYTES.min(config.max_unit_bytes),
            language_pending: false,
            language_eof: false,
        }
    }

    fn push(&mut self, offset: usize, character: char) {
        debug_assert!(self.ready.is_empty());
        match self.mode {
            ResolvedMode::Window => self.append_character(offset, character),
            ResolvedMode::Paragraph => self.push_paragraph(offset, character),
            ResolvedMode::Section => self.push_section(offset, character),
            ResolvedMode::Language(_) => self.push_language(offset, character),
        }
        self.remember(character);
        if character == '\n' {
            self.line += 1;
            self.line_has_content = false;
            if self.mode == ResolvedMode::Section {
                self.section_line = match self.fence {
                    Some((marker, minimum)) => {
                        SectionLine::Fenced(FenceTracker::new(marker, minimum))
                    }
                    None => SectionLine::Probe(Vec::with_capacity(10)),
                };
            }
        } else if !character.is_whitespace() {
            self.line_has_content = true;
        }
    }

    fn push_language(&mut self, offset: usize, character: char) {
        self.append_character(offset, character);
        if character == '\n'
            || self
                .builder
                .as_ref()
                .is_some_and(|builder| builder.bytes.len() >= self.next_probe)
        {
            self.language_pending = true;
        }
    }

    fn split_language(&mut self, eof: bool) {
        let ResolvedMode::Language(mode) = self.mode else {
            return;
        };
        let boundary = match mode {
            UnitMode::Javascript => super::segmenters::javascript,
            UnitMode::Rust => super::segmenters::rust,
            UnitMode::Css => super::segmenters::css,
            UnitMode::Prose => super::segmenters::prose,
            _ => unreachable!("language mode"),
        };
        self.language_pending = false;
        if let Some(mut builder) = self.builder.take() {
            let text = std::str::from_utf8(&builder.bytes).expect("valid UTF-8 buffer");
            let Some(end) = boundary(text, eof).filter(|end| *end > 0) else {
                let retained = builder.bytes.len();
                self.next_probe = retained
                    .saturating_mul(2)
                    .max(BUFFER_BYTES)
                    .min(self.config.max_unit_bytes);
                self.builder = Some(builder);
                return;
            };
            let suffix = builder.bytes.split_off(end);
            self.next_probe = BUFFER_BYTES.min(self.config.max_unit_bytes);
            if !suffix.is_empty() {
                self.language_pending = true;
                self.builder = Some(UnitBuffer {
                    start_byte: builder.start_byte + end,
                    start_line: builder.start_line
                        + builder.bytes.iter().filter(|b| **b == b'\n').count(),
                    context: bounded_context(&builder.context, &builder.bytes, self.context_limit),
                    bytes: suffix,
                });
            }
            if let Some(unit) = make_unit(&self.path, &builder, language_kind(mode), Vec::new()) {
                self.ready.push_back(unit);
            }
        }
    }

    fn push_paragraph(&mut self, offset: usize, character: char) {
        if character == '\n' && !self.line_has_content {
            self.flush(false);
        } else {
            self.append_character(offset, character);
        }
    }

    fn push_section(&mut self, offset: usize, character: char) {
        let state = std::mem::replace(&mut self.section_line, SectionLine::Plain);
        match state {
            SectionLine::Probe(mut probe) => {
                probe.push(CharToken { offset, character });
                match classify_probe(&probe) {
                    ProbeDecision::More => self.section_line = SectionLine::Probe(probe),
                    ProbeDecision::Plain => {
                        for token in probe {
                            self.append_character(token.offset, token.character);
                        }
                        self.section_line = SectionLine::Plain;
                    }
                    ProbeDecision::Heading(level) => {
                        self.flush(false);
                        while self
                            .hierarchy
                            .last()
                            .is_some_and(|(parent, _)| *parent >= level)
                        {
                            self.hierarchy.pop();
                        }
                        self.hierarchy.push((level, String::new()));
                        self.heading_raw.clear();
                        for token in probe {
                            self.append_character(token.offset, token.character);
                        }
                        self.section_line = SectionLine::Heading;
                    }
                    ProbeDecision::Fence(marker, minimum) => {
                        self.fence = Some((marker, minimum));
                        self.opening_fence = Some(marker);
                        for token in probe {
                            self.append_character(token.offset, token.character);
                        }
                        self.section_line = SectionLine::Plain;
                    }
                }
            }
            SectionLine::Plain => {
                if let Some(marker) = self.opening_fence {
                    if character == marker {
                        if let Some((_, minimum)) = &mut self.fence {
                            *minimum += 1;
                        }
                    } else {
                        self.opening_fence = None;
                    }
                }
                self.append_character(offset, character);
            }
            SectionLine::Heading => {
                self.append_character(offset, character);
                if character == '\n' {
                    self.finish_heading();
                } else {
                    if push_bounded(&mut self.heading_raw, character, MAX_HEADING_BYTES) {
                        self.update_heading(false);
                    }
                    self.section_line = SectionLine::Heading;
                }
            }
            SectionLine::Fenced(mut tracker) => {
                if character == '\n' {
                    if tracker.closes() {
                        self.fence = None;
                    }
                } else {
                    tracker.push(character);
                }
                self.append_character(offset, character);
                self.section_line = SectionLine::Fenced(tracker);
            }
        }
    }

    fn finish_heading(&mut self) {
        self.update_heading(true);
    }

    fn update_heading(&mut self, final_value: bool) {
        let value = normalized_heading(&self.heading_raw, final_value);
        if let Some((_, heading)) = self.hierarchy.last_mut() {
            heading.clear();
            heading.push_str(value);
        }
    }

    fn append_character(&mut self, offset: usize, character: char) {
        let mut encoded_buffer = [0; 4];
        let encoded = character.encode_utf8(&mut encoded_buffer).as_bytes();
        loop {
            let needs_flush = self.builder.as_ref().is_some_and(|builder| {
                builder.bytes.len().saturating_add(encoded.len()) > self.config.max_unit_bytes
                    || self.line.saturating_sub(builder.start_line) >= self.config.window_lines
            });
            if !needs_flush {
                break;
            }
            self.flush(true);
        }

        if self.builder.is_none() {
            if character.is_whitespace() {
                return;
            }
            self.builder = Some(UnitBuffer {
                start_byte: offset,
                start_line: self.line,
                bytes: Vec::with_capacity(self.config.max_unit_bytes.min(BUFFER_BYTES)),
                context: self.context_snapshot(),
            });
        }
        self.builder
            .as_mut()
            .unwrap()
            .bytes
            .extend_from_slice(encoded);
    }

    fn flush(&mut self, overlap: bool) {
        let Some(builder) = self.builder.take() else {
            return;
        };
        let overlap_start =
            overlap.then(|| overlap_start(&builder.bytes, self.config.overlap_lines));
        let headings = self
            .hierarchy
            .iter()
            .map(|(_, heading)| heading.clone())
            .collect();
        let kind = match self.mode {
            ResolvedMode::Section => "Markdown section",
            ResolvedMode::Paragraph => "paragraph",
            ResolvedMode::Window => "text window",
            ResolvedMode::Language(mode) => language_kind(mode),
        };
        let unit = make_unit(&self.path, &builder, kind, headings);

        if let Some(start) =
            overlap_start.filter(|start| *start > 0 && *start < builder.bytes.len())
        {
            let start_line = builder.start_line
                + builder.bytes[..start]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count();
            self.builder = Some(UnitBuffer {
                start_byte: builder.start_byte + start,
                start_line,
                bytes: builder.bytes[start..].to_vec(),
                context: bounded_context(
                    &builder.context,
                    &builder.bytes[..start],
                    self.context_limit,
                ),
            });
        }
        if let Some(unit) = unit {
            debug_assert!(self.ready.len() < 2);
            self.ready.push_back(unit);
        }
    }

    fn remember(&mut self, character: char) {
        let mut encoded = [0; 4];
        self.history
            .extend(character.encode_utf8(&mut encoded).as_bytes());
        while self.history.len() > self.context_limit {
            self.history.pop_front();
        }
    }

    fn context_snapshot(&self) -> String {
        let bytes: Vec<_> = self.history.iter().copied().collect();
        let start = bytes
            .iter()
            .position(|byte| byte & 0b1100_0000 != 0b1000_0000)
            .unwrap_or(bytes.len());
        String::from_utf8(bytes[start..].to_vec())
            .expect("history contains complete UTF-8 characters after its leading continuation")
    }

    fn take_ready(&mut self) -> Option<Unit> {
        while self.ready.is_empty() && self.language_pending {
            self.split_language(self.language_eof);
        }
        self.ready.pop_front()
    }

    fn finish(&mut self) {
        if matches!(self.mode, ResolvedMode::Language(_)) {
            self.language_eof = true;
            self.language_pending = true;
            return;
        }
        self.finish_probe();
        if matches!(self.section_line, SectionLine::Heading) {
            self.finish_heading();
        }
        self.flush(false);
    }

    fn abort(&mut self) {
        self.builder = None;
        self.ready.clear();
    }

    fn finish_probe(&mut self) {
        let SectionLine::Probe(probe) =
            std::mem::replace(&mut self.section_line, SectionLine::Plain)
        else {
            return;
        };
        let characters: Vec<_> = probe.iter().map(|token| token.character).collect();
        let spaces = characters
            .iter()
            .take_while(|character| **character == ' ')
            .count();
        let hashes = characters[spaces..]
            .iter()
            .take_while(|character| **character == '#')
            .count();
        let heading =
            hashes > 0 && hashes <= 6 && spaces <= 3 && spaces + hashes == characters.len();
        if heading {
            self.flush(false);
            while self
                .hierarchy
                .last()
                .is_some_and(|(parent, _)| *parent >= hashes)
            {
                self.hierarchy.pop();
            }
            self.hierarchy.push((hashes, String::new()));
            self.heading_raw.clear();
            for token in probe {
                self.append_character(token.offset, token.character);
            }
            self.section_line = SectionLine::Heading;
        } else {
            for token in probe {
                self.append_character(token.offset, token.character);
            }
            self.section_line = SectionLine::Plain;
        }
    }
}

fn classify_probe(probe: &[CharToken]) -> ProbeDecision {
    let characters: Vec<_> = probe.iter().map(|token| token.character).collect();
    let spaces = characters
        .iter()
        .take_while(|character| **character == ' ')
        .count();
    if spaces > 3 {
        return ProbeDecision::Plain;
    }
    let Some(first) = characters.get(spaces).copied() else {
        return ProbeDecision::More;
    };
    if first == '\t' {
        return ProbeDecision::Plain;
    }
    if first == '#' {
        let count = characters[spaces..]
            .iter()
            .take_while(|character| **character == '#')
            .count();
        if count > 6 {
            return ProbeDecision::Plain;
        }
        return match characters.get(spaces + count) {
            None => ProbeDecision::More,
            Some(character) if character.is_whitespace() => ProbeDecision::Heading(count),
            Some(_) => ProbeDecision::Plain,
        };
    }
    if matches!(first, '`' | '~') {
        let count = characters[spaces..]
            .iter()
            .take_while(|character| **character == first)
            .count();
        return if count >= 3 {
            ProbeDecision::Fence(first, count)
        } else if characters.len() == spaces + count {
            ProbeDecision::More
        } else {
            ProbeDecision::Plain
        };
    }
    ProbeDecision::Plain
}

fn push_bounded(value: &mut String, character: char, max_bytes: usize) -> bool {
    if value.len().saturating_add(character.len_utf8()) <= max_bytes {
        value.push(character);
        true
    } else {
        false
    }
}

fn normalized_heading(value: &str, final_value: bool) -> &str {
    let mut value = value.trim();
    if final_value {
        let hashes = value.bytes().rev().take_while(|byte| *byte == b'#').count();
        if hashes > 0 {
            let start = value.len() - hashes;
            if start > 0 && value[..start].ends_with(char::is_whitespace) {
                value = value[..start].trim_end();
            }
        }
    }
    value
}

fn make_unit(path: &Path, builder: &UnitBuffer, kind: &str, headings: Vec<String>) -> Option<Unit> {
    let text = std::str::from_utf8(&builder.bytes).expect("unit bytes are valid UTF-8");
    let trimmed_start = text.trim_start_matches(char::is_whitespace);
    let leading = text.len() - trimmed_start.len();
    let target = trimmed_start.trim_end_matches(char::is_whitespace);
    if target.is_empty() {
        return None;
    }
    let start_line = builder.start_line
        + builder.bytes[..leading]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
    let end_line = start_line + target.bytes().filter(|byte| *byte == b'\n').count();
    Some(Unit {
        path: path.to_owned(),
        start_byte: builder.start_byte + leading,
        end_byte: builder.start_byte + leading + target.len(),
        start_line,
        end_line,
        kind: kind.to_owned(),
        headings,
        target: target.to_owned(),
        context: builder.context.clone(),
    })
}

fn overlap_start(bytes: &[u8], lines: usize) -> usize {
    if lines == 0 {
        return bytes.len();
    }
    let mut needed = lines;
    let mut end = bytes.len();
    if bytes.last() == Some(&b'\n') {
        end -= 1;
    }
    for index in (0..end).rev() {
        if bytes[index] == b'\n' {
            needed -= 1;
            if needed == 0 {
                return index + 1;
            }
        }
    }
    0
}

fn bounded_context(prefix: &str, bytes: &[u8], max_bytes: usize) -> String {
    let mut combined = Vec::with_capacity(prefix.len().saturating_add(bytes.len()));
    combined.extend_from_slice(prefix.as_bytes());
    combined.extend_from_slice(bytes);
    let mut start = combined.len().saturating_sub(max_bytes);
    while start < combined.len() && combined[start] & 0b1100_0000 == 0b1000_0000 {
        start += 1;
    }
    String::from_utf8(combined[start..].to_vec())
        .expect("context is sliced at a UTF-8 character boundary")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn config(mode: UnitMode, max_unit_bytes: usize) -> ScanConfig {
        ScanConfig {
            max_file_bytes: 2_000_000,
            max_unit_bytes,
            window_lines: 3,
            overlap_lines: 1,
            unit_mode: mode,
        }
    }

    fn units(source: &[u8], path: &str, config: ScanConfig) -> (Vec<Unit>, Option<String>, bool) {
        let mut scanner = FileScanner::new(Cursor::new(source.to_vec()), path.into(), config);
        let mut units = Vec::new();
        loop {
            match scanner.next_event() {
                FileEvent::Unit(unit) => units.push(unit),
                FileEvent::Progress { .. } => {}
                FileEvent::Finished(_) => return (units, None, true),
                FileEvent::Issue(issue) => return (units, Some(issue.message), false),
            }
        }
    }

    #[test]
    fn markdown_stream_keeps_hierarchy_and_ignores_fenced_headings() {
        let source = b"Preamble.\n\n# Act\nIntro.\n\n### Rule\nText.\n```md\n# not heading\n``` not close\n# still fenced\n```\n\n### Exception\nExcept.\n";
        let mut scan_config = config(UnitMode::Section, 1_000);
        scan_config.window_lines = 50;
        let (units, issue, finished) = units(source, "law.md", scan_config);
        assert!(finished);
        assert_eq!(issue, None);
        assert_eq!(units.len(), 4);
        assert!(units[0].headings.is_empty());
        assert_eq!(units[2].headings, ["Act", "Rule"]);
        assert!(units[2].target.contains("# still fenced"));
        assert_eq!(units[3].headings, ["Act", "Exception"]);
    }

    #[test]
    fn paragraphs_stream_arbitrary_text() {
        let source = b"first paragraph\ncontinued\n\nsecond paragraph\n";
        let (units, issue, finished) =
            units(source, "notes.txt", config(UnitMode::Paragraph, 1_000));
        assert!(finished);
        assert_eq!(issue, None);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].target, "first paragraph\ncontinued");
        assert_eq!(units[1].target, "second paragraph");
    }

    #[test]
    fn giant_minified_line_stays_bounded_and_byte_exact() {
        let source = "🦀".repeat(25_000);
        let (units, issue, finished) =
            units(source.as_bytes(), "giant.txt", config(UnitMode::Window, 17));
        assert!(finished);
        assert_eq!(issue, None);
        assert!(units.len() > 1);
        for unit in &units {
            assert!(unit.target.len() <= 17);
            assert_eq!(
                &source.as_bytes()[unit.start_byte..unit.end_byte],
                unit.target.as_bytes()
            );
        }
        assert_eq!(
            units.iter().map(|unit| unit.target.len()).sum::<usize>(),
            source.len()
        );
    }

    #[test]
    fn oversized_section_overlaps_by_line_and_keeps_heading_state() {
        let source = b"# Law\none\ntwo\nthree\nfour\nfive\n";
        let (units, issue, finished) = units(source, "law.md", config(UnitMode::Section, 1_000));
        assert!(finished);
        assert_eq!(issue, None);
        assert_eq!(units.len(), 3);
        assert_eq!((units[0].start_line, units[0].end_line), (1, 3));
        assert_eq!((units[1].start_line, units[1].end_line), (3, 5));
        assert_eq!((units[2].start_line, units[2].end_line), (5, 6));
        assert!(units.iter().all(|unit| unit.headings == ["Law"]));
    }

    #[test]
    fn byte_spans_cover_every_non_whitespace_source_byte() {
        let source = b"alpha\n\nbeta gamma\n\ndelta\n";
        let (units, issue, finished) =
            units(source, "notes.txt", config(UnitMode::Paragraph, 1_000));
        assert!(finished);
        assert_eq!(issue, None);
        for unit in &units {
            assert_eq!(
                &source[unit.start_byte..unit.end_byte],
                unit.target.as_bytes()
            );
        }
        for (index, byte) in source.iter().enumerate() {
            if !byte.is_ascii_whitespace() {
                assert!(
                    units
                        .iter()
                        .any(|unit| (unit.start_byte..unit.end_byte).contains(&index))
                );
            }
        }
    }

    #[test]
    fn whitespace_only_input_yields_progress_checkpoints() {
        let source = vec![b' '; BUFFER_BYTES * 3];
        let mut scanner = FileScanner::new(
            Cursor::new(source),
            "spaces.txt".into(),
            config(UnitMode::Paragraph, 64),
        );
        let FileEvent::Progress { bytes_scanned, .. } = scanner.next_event() else {
            panic!("expected bounded progress checkpoint");
        };
        assert_eq!(bytes_scanned, BUFFER_BYTES as u64);
    }

    #[test]
    fn overlap_that_cannot_fit_multibyte_character_preserves_all_ready_units() {
        let source = format!("a\n{}🦀", "x".repeat(13));
        let mut scan_config = config(UnitMode::Window, 16);
        scan_config.window_lines = 10;
        let (units, issue, finished) = units(source.as_bytes(), "mixed.txt", scan_config);
        assert!(finished);
        assert_eq!(issue, None);
        assert!(units.iter().all(|unit| unit.target.len() <= 16));
        for unit in &units {
            assert_eq!(
                &source.as_bytes()[unit.start_byte..unit.end_byte],
                unit.target.as_bytes()
            );
        }
        for (index, byte) in source.bytes().enumerate() {
            if !byte.is_ascii_whitespace() {
                assert!(
                    units
                        .iter()
                        .any(|unit| (unit.start_byte..unit.end_byte).contains(&index))
                );
            }
        }
    }

    #[test]
    fn eof_heading_probe_preserves_previous_and_final_units() {
        let source = b"preface\n#";
        let mut scan_config = config(UnitMode::Section, 100);
        scan_config.window_lines = 10;
        let (units, issue, finished) = units(source, "law.md", scan_config);
        assert!(finished);
        assert_eq!(issue, None);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].target, "preface");
        assert_eq!(units[1].target, "#");
        assert_eq!(units[0].start_byte, 0);
        assert_eq!(units[1].start_byte, 8);
    }

    #[test]
    fn four_character_fence_is_not_closed_by_three_characters() {
        let source = b"````\n```\n# should remain code\n````\n# real\ntext\n";
        let mut scan_config = config(UnitMode::Section, 1_000);
        scan_config.window_lines = 50;
        let (units, issue, finished) = units(source, "law.md", scan_config);
        assert!(finished);
        assert_eq!(issue, None);
        assert_eq!(units.len(), 2);
        assert!(units[0].target.contains("# should remain code"));
        assert_eq!(units[1].headings, ["real"]);
    }

    #[test]
    fn invalid_utf8_after_emitted_units_is_an_issue_without_file_finished() {
        let mut source = vec![b'a'; BUFFER_BYTES + 10];
        source.push(0xff);
        let (units, issue, finished) = units(&source, "broken.txt", config(UnitMode::Window, 32));
        assert!(!units.is_empty());
        assert!(!finished);
        assert!(issue.unwrap().contains("invalid UTF-8"));
    }

    #[derive(Clone)]
    struct CountingReader {
        bytes: Arc<Vec<u8>>,
        position: usize,
        read: Arc<AtomicUsize>,
    }

    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let available = &self.bytes[self.position..];
            let length = available.len().min(buffer.len());
            buffer[..length].copy_from_slice(&available[..length]);
            self.position += length;
            self.read.fetch_add(length, Ordering::SeqCst);
            Ok(length)
        }
    }

    #[test]
    fn first_unit_does_not_read_the_whole_file_and_drop_stops_reads() {
        let bytes = Arc::new(vec![b'x'; 1_000_000]);
        let read = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            bytes: Arc::clone(&bytes),
            position: 0,
            read: Arc::clone(&read),
        };
        let mut scanner =
            FileScanner::new(reader, "large.txt".into(), config(UnitMode::Window, 64));
        assert!(matches!(scanner.next_event(), FileEvent::Unit(_)));
        let consumed = read.load(Ordering::SeqCst);
        assert!(consumed <= BUFFER_BYTES);
        assert!(consumed < bytes.len());
        drop(scanner);
        assert_eq!(read.load(Ordering::SeqCst), consumed);
    }

    #[test]
    fn scanner_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Scanner>();
    }

    #[cfg(unix)]
    #[test]
    fn cache_exclusion_resolves_aliases_before_cache_creation() {
        let directory = crate::tests::TestDirectory::new();
        let corpus = directory.0.join("corpus");
        fs::create_dir(&corpus).unwrap();
        fs::write(corpus.join("source.txt"), "source").unwrap();
        let alias = directory.0.join("alias");
        std::os::unix::fs::symlink(&corpus, &alias).unwrap();
        let excluded = alias.join("cache");
        let scanner = Scanner::new(
            std::slice::from_ref(&corpus),
            config(UnitMode::Paragraph, 256),
            Some(&excluded),
        );
        fs::create_dir(&excluded).unwrap();
        fs::write(excluded.join("entry.json"), "0.9").unwrap();
        let discovered: Vec<_> = scanner
            .filter_map(|event| match event {
                ScanEvent::FileStarted(path) => Some(path),
                _ => None,
            })
            .collect();
        assert_eq!(discovered, [corpus.join("source.txt")]);
    }

    #[test]
    fn excluded_directory_is_pruned_and_events_are_incremental() {
        let root = std::env::temp_dir().join(format!(
            "tsg-streaming-scanner-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = root.join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(root.join("visible.txt"), "visible").unwrap();
        fs::write(cache.join("entry.txt"), "must not scan").unwrap();

        let scanner = Scanner::new(
            std::slice::from_ref(&root),
            config(UnitMode::Paragraph, 100),
            Some(&cache),
        );
        let events: Vec<_> = scanner.collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ScanEvent::Unit(unit) if unit.target == "visible"))
        );
        assert!(!events.iter().any(
            |event| matches!(event, ScanEvent::Unit(unit) if unit.target.contains("must not"))
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ScanEvent::FileStarted(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ScanEvent::FileFinished(_)))
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unicode_sentences_survive_buffer_boundaries_and_keep_exact_spans() {
        use unicode_segmentation::UnicodeSegmentation;
        let source = format!(
            "{}3.14 is a decimal. Next sentence! 你好。 Last",
            "x".repeat(BUFFER_BYTES - 3)
        );
        let (actual, issue, finished) = units(
            source.as_bytes(),
            "misleading.rs",
            config(UnitMode::Prose, BUFFER_BYTES * 3),
        );
        assert!(finished);
        assert_eq!(issue, None);
        let expected: Vec<_> = source
            .split_sentence_bounds()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            actual
                .iter()
                .map(|unit| unit.target.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        for unit in actual {
            assert_eq!(&source[unit.start_byte..unit.end_byte], unit.target);
        }
    }

    #[test]
    fn language_caps_preserve_every_non_whitespace_byte() {
        let source = "🦀abcd\r\nfn x() { do_it(); }\n你好。 More!";
        for mode in [
            UnitMode::Javascript,
            UnitMode::Rust,
            UnitMode::Css,
            UnitMode::Prose,
        ] {
            let (actual, issue, finished) = units(source.as_bytes(), "input", config(mode, 9));
            assert!(finished);
            assert_eq!(issue, None);
            for unit in &actual {
                assert!(unit.target.len() <= 9);
                assert_eq!(&source[unit.start_byte..unit.end_byte], unit.target);
                assert_eq!(
                    unit.start_line,
                    source[..unit.start_byte]
                        .bytes()
                        .filter(|b| *b == b'\n')
                        .count()
                        + 1
                );
            }
            for (offset, ch) in source.char_indices().filter(|(_, ch)| !ch.is_whitespace()) {
                assert!(
                    actual
                        .iter()
                        .any(|u| u.start_byte <= offset && u.end_byte >= offset + ch.len_utf8())
                );
            }
        }
    }

    #[test]
    fn completed_code_lines_segment_before_the_shared_line_cap() {
        for statement in ["work();\n", "record()\n", "record(\n  value\n)\n"] {
            let source = statement.repeat(100);
            let mut scan_config = config(UnitMode::Javascript, 16_384);
            scan_config.window_lines = 80;
            let (actual, issue, finished) = units(source.as_bytes(), "code", scan_config);
            assert!(finished);
            assert_eq!(issue, None);
            assert_eq!(actual.len(), 100);
            assert!(actual.iter().all(|unit| unit.target == statement.trim()));
        }
    }

    #[test]
    fn dense_sentences_are_lazy_and_language_units_respect_line_caps() {
        let source = "A! ".repeat(10_000);
        let mut scanner = FileScanner::new(
            Cursor::new(source),
            "dense".into(),
            config(UnitMode::Prose, 100_000),
        );
        loop {
            match scanner.next_event() {
                FileEvent::Unit(_) => assert!(scanner.segmenter.ready.len() <= 2),
                FileEvent::Finished(_) => break,
                FileEvent::Progress { .. } => {}
                FileEvent::Issue(issue) => panic!("{issue:?}"),
            }
        }
        for mode in [
            UnitMode::Javascript,
            UnitMode::Rust,
            UnitMode::Css,
            UnitMode::Prose,
        ] {
            let (actual, issue, finished) =
                units(b"a\nb\nc\nd\ne\nf\ng\n", "lines", config(mode, 1_000));
            assert!(finished);
            assert_eq!(issue, None);
            assert!(actual.len() > 1);
            assert!(
                actual
                    .iter()
                    .all(|unit| unit.end_line - unit.start_line < 3)
            );
        }
    }

    #[test]
    fn file_discovery_is_lazy_and_children_scan_independent_files() {
        let directory = crate::tests::TestDirectory::new();
        fs::write(directory.0.join("first"), "initial").unwrap();
        fs::write(directory.0.join("second"), "initial").unwrap();
        let mut discovery = Scanner::new(
            std::slice::from_ref(&directory.0),
            config(UnitMode::Paragraph, 64),
            None,
        );
        let first = discovery.next_file().unwrap().unwrap();
        assert!(first.current.is_none());
        assert!(first.snapshot.is_none());
        assert!(discovery.current.is_none());
        assert!(discovery.snapshot.is_none());
        // Discovering a child must not snapshot its original contents.
        fs::write(directory.0.join("first"), "changed").unwrap();
        fs::write(directory.0.join("second"), "changed").unwrap();
        let second = discovery.next_file().unwrap().unwrap();
        assert!(discovery.next_file().is_none());
        let paths: Vec<_> = [first, second].into_iter().map(|child| {
            let events: Vec<_> = child.collect();
            let starts: Vec<_> = events.iter().filter_map(|event| match event {
                ScanEvent::FileStarted(path) => Some(path.clone()),
                _ => None,
            }).collect();
            assert_eq!(starts.len(), 1);
            assert_eq!(events.iter().filter(|event| matches!(event, ScanEvent::Unit(unit) if unit.target == "changed")).count(), 1);
            starts[0].clone()
        }).collect();
        assert_ne!(paths[0], paths[1]);
    }

    #[test]
    fn malformed_independent_file_finishes_with_one_issue() {
        let directory = crate::tests::TestDirectory::new();
        let path = directory.0.join("invalid");
        fs::write(&path, b"bad\xff").unwrap();
        let mut discovery = Scanner::new(
            std::slice::from_ref(&path),
            config(UnitMode::Auto, 64),
            None,
        );
        let mut child = discovery.next_file().unwrap().unwrap();
        assert!(matches!(child.next(), Some(ScanEvent::FileStarted(_))));
        assert!(matches!(child.next(), Some(ScanEvent::Issue(_))));
        assert!(child.snapshot.is_none());
        assert!(child.next().is_none());
    }

    #[test]
    fn routing_parks_replays_original_snapshot_and_cleans_up() {
        let directory = crate::tests::TestDirectory::new();
        let path = directory.0.join("misleading.rs");
        let original = "One sentence. Another sentence!";
        fs::write(&path, original).unwrap();
        let mut scanner = Scanner::new(
            std::slice::from_ref(&path),
            config(UnitMode::Auto, 16),
            None,
        );
        assert!(matches!(scanner.next(), Some(ScanEvent::FileStarted(_))));
        let snapshot_path = scanner.snapshot.as_ref().unwrap().path.clone();
        let mut chunks = Vec::new();
        loop {
            match scanner.next().unwrap() {
                ScanEvent::RoutingChunk(unit) => chunks.push(unit),
                ScanEvent::Progress { .. } => {}
                ScanEvent::RoutingReady(_) => break,
                event => panic!("unexpected routing event {event:?}"),
            }
        }
        assert!(chunks.len() > 1);
        for pair in chunks.windows(2) {
            assert!(pair[0].end_byte <= pair[1].start_byte);
        }
        assert!(scanner.next().is_none());
        fs::write(&path, "REPLACED").unwrap();
        scanner.select_mode(UnitMode::Prose).unwrap();
        let actual: Vec<_> = scanner
            .by_ref()
            .filter_map(|event| match event {
                ScanEvent::Unit(unit) => Some(unit),
                ScanEvent::FileFinished(_) | ScanEvent::Progress { .. } => None,
                other => panic!("unexpected replay event {other:?}"),
            })
            .collect();
        assert!(!actual.is_empty());
        for unit in actual {
            assert_eq!(&original[unit.start_byte..unit.end_byte], unit.target);
        }
        assert!(!snapshot_path.exists());
    }

    #[test]
    fn routing_snapshot_is_removed_on_drop_skip_and_invalid_input() {
        for finish in [0, 1, 2] {
            let directory = crate::tests::TestDirectory::new();
            let path = directory.0.join("input");
            fs::write(
                &path,
                if finish == 2 {
                    b"bad\xff".as_slice()
                } else {
                    b"valid".as_slice()
                },
            )
            .unwrap();
            let mut scanner = Scanner::new(
                std::slice::from_ref(&path),
                config(UnitMode::Auto, 32),
                None,
            );
            scanner.next();
            let snapshot_path = scanner.snapshot.as_ref().unwrap().path.clone();
            assert!(snapshot_path.exists());
            match finish {
                0 => drop(scanner),
                1 => scanner.skip_routing(),
                _ => assert!(scanner.any(|event| matches!(event, ScanEvent::Issue(_)))),
            }
            assert!(!snapshot_path.exists());
        }
    }

    static NEXT_TEST: AtomicUsize = AtomicUsize::new(0);
}
