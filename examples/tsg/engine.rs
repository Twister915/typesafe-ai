use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::{
    Stream, StreamExt,
    stream::{self, FuturesUnordered},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use typesafe_ai::{Answer, NoulCriteria, Question, Request, ReqwestClient};

use crate::cache::Cache;
use crate::routing::{self, Decision, Evidence};
use crate::scanner::{ScanEvent, ScanIssue, Scanner, Unit, UnitMode};

const QUESTION_ID: &str = "target_matches";
const PROMPT_VERSION: &str = "tsg-stream-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    Find,
    Grep,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Match {
    #[serde(flatten)]
    pub(crate) unit: Unit,
    pub(crate) probability: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EvaluationFailure {
    pub(crate) path: PathBuf,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct EvaluationCoverage {
    pub(crate) files_discovered: usize,
    pub(crate) files_scanned: usize,
    pub(crate) scan_issues: usize,
    pub(crate) discovery_complete: bool,
    pub(crate) units_total: usize,
    pub(crate) units_succeeded: usize,
    pub(crate) units_failed: usize,
    pub(crate) units_cancelled: usize,
    pub(crate) units_previewed: usize,
    pub(crate) cache_hits: usize,
    pub(crate) evaluations_scheduled: usize,
    pub(crate) routing_windows: usize,
    pub(crate) routing_succeeded: usize,
    pub(crate) routing_failed: usize,
    pub(crate) routing_cancelled: usize,
    pub(crate) routing_previewed: usize,
    pub(crate) routing_cache_hits: usize,
    pub(crate) files_routed: usize,
}

impl EvaluationCoverage {
    pub(crate) fn complete(&self) -> bool {
        self.discovery_complete
            && self.files_scanned == self.files_discovered
            && self.scan_issues == 0
            && self.units_failed == 0
            && self.units_cancelled == 0
            && self.routing_failed == 0
            && self.routing_cancelled == 0
    }
}

#[derive(Debug)]
pub(crate) enum PipelineEvent {
    Match(Match),
    Failure(EvaluationFailure),
    ScanIssue(ScanIssue),
    Request(Request),
    Routed(Decision),
    Finished(EvaluationCoverage),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EvaluationConfig<'a> {
    pub(crate) mode: Mode,
    pub(crate) query: &'a str,
    pub(crate) model: &'a str,
    pub(crate) concurrency: usize,
    pub(crate) cache_namespace: &'a str,
    pub(crate) dry_run: bool,
    pub(crate) progress: Option<&'a tokio::sync::watch::Sender<EvaluationCoverage>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Cancellation {
    inner: Arc<CancellationInner>,
}

#[derive(Debug, Default)]
struct CancellationInner {
    cancelled: std::sync::atomic::AtomicBool,
    notify: Notify,
}

impl Cancellation {
    pub(crate) fn cancel(&self) {
        self.inner
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire)
    }

    async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        tokio::pin!(notified);
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

pub(crate) trait Evaluator: Sync {
    fn evaluate(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<BTreeMap<String, Answer>, String>> + Send;
}

#[derive(Debug)]
pub(crate) struct ApiEvaluator {
    client: ReqwestClient,
}

impl ApiEvaluator {
    pub(crate) fn new(client: ReqwestClient) -> Self {
        Self { client }
    }
}

impl Evaluator for ApiEvaluator {
    async fn evaluate(&self, request: Request) -> Result<BTreeMap<String, Answer>, String> {
        self.client
            .evaluate(&request)
            .await
            .map(|response| response.answers)
            .map_err(|error| {
                if let Some(request_id) = error.request_id() {
                    format!("{error} (request id {request_id})")
                } else {
                    error.to_string()
                }
            })
    }
}

fn probability(answers: &BTreeMap<String, Answer>, id: &str) -> Result<f64, String> {
    let answer = answers
        .get(id)
        .ok_or_else(|| format!("response omitted answer {id:?}"))?;
    let Answer::Noul { noul } = answer else {
        return Err(format!(
            "response answer {id:?} had type {:?}, expected noul",
            answer_type(answer)
        ));
    };
    validate_probability(*noul)
}

fn answer_type(answer: &Answer) -> &'static str {
    match answer {
        Answer::Noul { .. } => "noul",
        Answer::Choice { .. } => "choice",
        Answer::Score { .. } => "score",
    }
}

fn validate_probability(probability: f64) -> Result<f64, String> {
    if probability.is_finite() && (0.0..=1.0).contains(&probability) {
        Ok(probability)
    } else {
        Err(format!(
            "response probability must be finite and between 0 and 1, received {probability}"
        ))
    }
}

pub(crate) fn request_for(mode: Mode, query: &str, model: &str, unit: &Unit) -> Request {
    let (instructions, when_true, when_false) = match mode {
        Mode::Grep => (
            "Judge whether `target` satisfies the semantic condition in `query`, using `headings` and `context` to interpret the target. Apply the relationship requested by the query exactly: a mention or comment counts when the query asks about mentioning or documenting something, while discussion alone does not establish implemented behavior when the query asks what code does.",
            "The target has the meaning or relationship requested by the query, interpreted with its headings and local context.",
            "The target does not have the meaning or relationship requested by the query, or the supplied evidence is insufficient.",
        ),
        Mode::Find => (
            "Could `target` help a reader investigate `query`, whether the query is a question, topic, or task? Judge it as independently retrievable source evidence, using `headings` and `context` only to interpret it. Relevant evidence can include direct applicability, requirements, constraints, definitions, exceptions, dependencies, or references. Do not resolve external references or make legal conclusions beyond the supplied text.",
            "The target provides specific evidence useful to the query, including an applicable rule, definition, constraint, exception, dependency, or reference.",
            "The target is unrelated, merely shares vocabulary, or lacks evidence useful for investigating the query.",
        ),
    };
    Request::new(json!({
        "prompt_version": PROMPT_VERSION,
        "query": query,
        "path": unit.path,
        "line_range": {"start": unit.start_line, "end": unit.end_line},
        "byte_range": {"start": unit.start_byte, "end": unit.end_byte},
        "kind": unit.kind,
        "headings": unit.headings,
        "target": unit.target,
        "context": unit.context,
    }))
    .with_model(model)
    .with_question(
        QUESTION_ID,
        Question::noul_with_criteria(instructions, NoulCriteria::new(when_true, when_false)),
    )
}

struct Pipeline<F> {
    discovery: Option<Box<Scanner>>,
    files: BTreeMap<usize, FileState>,
    ready: std::collections::VecDeque<usize>,
    reader: Option<tokio::task::JoinHandle<ReadOutput>>,
    reader_file: Option<usize>,
    pending: FuturesUnordered<F>,
    coverage: EvaluationCoverage,
    input_finished: bool,
    discovery_failed: bool,
    finished: bool,
    next_file_id: usize,
    discover_next: bool,
}

struct FileState {
    scanner: Option<Box<Scanner>>,
    routing: RoutingState,
    selection: Option<UnitMode>,
}

#[derive(Debug, Default)]
struct RoutingState {
    evidence: Evidence,
    pending: usize,
    ready: Option<PathBuf>,
    failure: Option<String>,
}

enum ReadOutput {
    Discovered(Box<Scanner>, Option<Result<Box<Scanner>, ScanIssue>>),
    File {
        id: usize,
        scanner: Box<Scanner>,
        event: Option<ScanEvent>,
    },
}

enum Work {
    Unit(Unit),
    Routing { unit: Unit, file_id: usize },
}

enum Outcome {
    Unit(Unit, Result<f64, String>, bool),
    Routing {
        file_id: usize,
        bytes: usize,
        result: Result<[f64; 4], String>,
        cached: bool,
    },
}

impl<F> Drop for Pipeline<F> {
    fn drop(&mut self) {
        if let Some(reader) = &self.reader {
            reader.abort();
        }
    }
}

pub(crate) fn pipeline<'a, E>(
    evaluator: Option<&'a E>,
    scanner: Scanner,
    config: EvaluationConfig<'a>,
    cache: &'a Cache,
    cancellation: Cancellation,
) -> impl Stream<Item = PipelineEvent> + 'a
where
    E: Evaluator,
{
    let state = Pipeline {
        discovery: Some(Box::new(scanner)),
        files: BTreeMap::new(),
        ready: Default::default(),
        reader: None,
        reader_file: None,
        pending: FuturesUnordered::new(),
        coverage: EvaluationCoverage::default(),
        input_finished: false,
        discovery_failed: false,
        finished: false,
        next_file_id: 0,
        discover_next: true,
    };
    stream::unfold(state, move |mut state| {
        let cancellation = cancellation.clone();
        async move {
            if state.finished {
                return None;
            }
            loop {
                if cancellation.is_cancelled() {
                    state.pending.clear();
                    if let Some(reader) = state.reader.take() {
                        reader.abort();
                    }
                    state.discovery = None;
                    state.files.clear();
                    state.ready.clear();
                    state.coverage.units_cancelled = state
                        .coverage
                        .units_total
                        .saturating_sub(state.coverage.units_succeeded)
                        .saturating_sub(state.coverage.units_failed)
                        .saturating_sub(state.coverage.units_previewed);
                    state.coverage.routing_cancelled = state
                        .coverage
                        .routing_windows
                        .saturating_sub(state.coverage.routing_succeeded)
                        .saturating_sub(state.coverage.routing_failed)
                        .saturating_sub(state.coverage.routing_previewed);
                    state.finished = true;
                    publish(config.progress, &state.coverage);
                    return Some((PipelineEvent::Finished(state.coverage.clone()), state));
                }

                // A completed file can start segmentation while other files are still
                // being discovered, read, or classified. There is no corpus-wide phase.
                let routed = state.files.iter().find_map(|(&id, file)| {
                    (file.routing.ready.is_some() && file.routing.pending == 0).then_some(id)
                });
                if let Some(id) = routed {
                    let file = state.files.get_mut(&id).expect("routed file");
                    let route = std::mem::take(&mut file.routing);
                    let path = route.ready.expect("completed routing read");
                    if let Some(message) = route.failure {
                        state.files.remove(&id);
                        state.coverage.scan_issues += 1;
                        publish(config.progress, &state.coverage);
                        return Some((
                            PipelineEvent::ScanIssue(ScanIssue {
                                path,
                                message: format!("segmenter routing failed: {message}"),
                            }),
                            state,
                        ));
                    }
                    let (mode, decision) = route.evidence.decide(path, config.dry_run);
                    if config.dry_run {
                        state.files.remove(&id);
                        state.coverage.files_scanned += 1;
                    } else {
                        file.selection = Some(mode);
                        state.ready.push_back(id);
                        state.coverage.files_routed += 1;
                    }
                    publish(config.progress, &state.coverage);
                    return Some((PipelineEvent::Routed(decision), state));
                }

                // One reader and all API/cache jobs share N slots. At most N file
                // states/snapshots are admitted; round-robin reads propagate backpressure.
                if state.reader.is_none() && state.pending.len() < config.concurrency {
                    let can_discover =
                        state.discovery.is_some() && state.files.len() < config.concurrency;
                    if can_discover && (state.discover_next || state.ready.is_empty()) {
                        let mut discovery = state.discovery.take().expect("available discovery");
                        state.reader_file = None;
                        state.reader = Some(tokio::task::spawn_blocking(move || {
                            let next = discovery.next_file().map(|file| file.map(Box::new));
                            ReadOutput::Discovered(discovery, next)
                        }));
                        state.discover_next = false;
                    } else if let Some(id) = state.ready.pop_front() {
                        let file = state.files.get_mut(&id).expect("ready file");
                        let mut scanner = file.scanner.take().expect("available file scanner");
                        let selection = file.selection.take();
                        state.reader_file = Some(id);
                        state.reader = Some(tokio::task::spawn_blocking(move || {
                            let event = if let Some(mode) = selection
                                && let Err(issue) = scanner.select_mode(mode)
                            {
                                Some(ScanEvent::Issue(issue))
                            } else {
                                scanner.next()
                            };
                            ReadOutput::File { id, scanner, event }
                        }));
                        state.discover_next = true;
                    }
                }
                if state.input_finished && state.files.is_empty() {
                    state.coverage.discovery_complete = !state.discovery_failed;
                    if state.pending.is_empty() {
                        state.finished = true;
                        publish(config.progress, &state.coverage);
                        return Some((PipelineEvent::Finished(state.coverage.clone()), state));
                    }
                }
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => continue,
                    Some(outcome) = state.pending.next(), if !state.pending.is_empty() => {
                        let event = match outcome {
                            Outcome::Unit(unit, result, cached) => match result {
                                Ok(probability) => {
                                    state.coverage.units_succeeded += 1;
                                    state.coverage.cache_hits += usize::from(cached);
                                    Some(PipelineEvent::Match(Match { unit, probability }))
                                }
                                Err(message) => {
                                    state.coverage.units_failed += 1;
                                    Some(PipelineEvent::Failure(EvaluationFailure {
                                        path: unit.path, start_line: unit.start_line,
                                        end_line: unit.end_line, message,
                                    }))
                                }
                            },
                            Outcome::Routing { file_id, bytes, result, cached } => {
                                state.coverage.routing_cache_hits += usize::from(cached);
                                if result.is_ok() { state.coverage.routing_succeeded += 1; }
                                else { state.coverage.routing_failed += 1; }
                                if let Some(file) = state.files.get_mut(&file_id) {
                                    file.routing.pending -= 1;
                                    match result {
                                        Ok(scores) => file.routing.evidence.add(bytes, scores),
                                        Err(message) => { file.routing.failure.get_or_insert(message); }
                                    }
                                }
                                None
                            }
                        };
                        publish(config.progress, &state.coverage);
                        if let Some(event) = event { return Some((event, state)); }
                    }
                    result = async { state.reader.as_mut().expect("guarded reader").await }, if state.reader.is_some() => {
                        state.reader = None;
                        let output = match result {
                            Ok(output) => output,
                            Err(_) => {
                                if let Some(id) = state.reader_file.take() { state.files.remove(&id); }
                                else { state.input_finished = true; state.discovery_failed = true; }
                                state.coverage.scan_issues += 1;
                                publish(config.progress, &state.coverage);
                                return Some((PipelineEvent::ScanIssue(ScanIssue {
                                    path: PathBuf::from("<scanner>"), message: "scanner task did not finish successfully".into(),
                                }), state));
                            }
                        };
                        let (id, event) = match output {
                            ReadOutput::Discovered(discovery, next) => {
                                match next {
                                    None => state.input_finished = true,
                                    Some(Ok(scanner)) => {
                                        state.discovery = Some(discovery);
                                        let id = state.next_file_id;
                                        state.next_file_id += 1;
                                        state.files.insert(id, FileState { scanner: Some(scanner), routing: Default::default(), selection: None });
                                        state.ready.push_back(id);
                                        state.coverage.files_discovered += 1;
                                    }
                                    Some(Err(issue)) => {
                                        state.discovery = Some(discovery);
                                        state.coverage.scan_issues += 1;
                                        publish(config.progress, &state.coverage);
                                        return Some((PipelineEvent::ScanIssue(issue), state));
                                    }
                                }
                                publish(config.progress, &state.coverage);
                                continue;
                            }
                            ReadOutput::File { id, scanner, event } => {
                                state.files.get_mut(&id).expect("reading file").scanner = Some(scanner);
                                (id, event)
                            }
                        };
                        match event {
                            None => { state.files.remove(&id); }
                            Some(ScanEvent::FileStarted(path)) => { drop(path); state.ready.push_back(id); }
                            Some(ScanEvent::FileFinished(path)) => {
                                drop(path); state.files.remove(&id); state.coverage.files_scanned += 1;
                            }
                            Some(ScanEvent::Progress { path, bytes_scanned }) => {
                                let _ = (path, bytes_scanned); state.ready.push_back(id);
                            }
                            Some(ScanEvent::Issue(issue)) => {
                                state.files.remove(&id);
                                state.coverage.scan_issues += 1;
                                publish(config.progress, &state.coverage);
                                return Some((PipelineEvent::ScanIssue(issue), state));
                            }
                            Some(ScanEvent::RoutingChunk(unit)) => {
                                state.ready.push_back(id);
                                state.coverage.routing_windows += 1;
                                let route = &mut state.files.get_mut(&id).expect("routing file").routing;
                                if config.dry_run {
                                    route.evidence.bytes += unit.end_byte - unit.start_byte;
                                    route.evidence.windows += 1;
                                    state.coverage.routing_previewed += 1;
                                    publish(config.progress, &state.coverage);
                                    return Some((PipelineEvent::Request(routing::request_for(config.model, &unit)), state));
                                }
                                route.pending += 1;
                                state.pending.push(evaluate_work(evaluator, Work::Routing { unit, file_id: id }, config, cache));
                            }
                            Some(ScanEvent::RoutingReady(path)) => {
                                state.files.get_mut(&id).expect("routing file").routing.ready = Some(path);
                            }
                            Some(ScanEvent::Unit(unit)) => {
                                state.ready.push_back(id);
                                state.coverage.units_total += 1;
                                if config.dry_run {
                                    state.coverage.units_previewed += 1;
                                    publish(config.progress, &state.coverage);
                                    return Some((PipelineEvent::Request(request_for(config.mode, config.query, config.model, &unit)), state));
                                }
                                state.coverage.evaluations_scheduled += 1;
                                state.pending.push(evaluate_work(evaluator, Work::Unit(unit), config, cache));
                            }
                        }
                        publish(config.progress, &state.coverage);
                    }
                }
            }
        }
    })
}

fn publish(
    sender: Option<&tokio::sync::watch::Sender<EvaluationCoverage>>,
    coverage: &EvaluationCoverage,
) {
    if let Some(sender) = sender {
        sender.send_replace(coverage.clone());
    }
}

async fn evaluate_work<E>(
    evaluator: Option<&E>,
    work: Work,
    config: EvaluationConfig<'_>,
    cache: &Cache,
) -> Outcome
where
    E: Evaluator,
{
    match work {
        Work::Unit(unit) => {
            let (unit, result, cached) = evaluate_unit(evaluator, unit, config, cache).await;
            Outcome::Unit(unit, result, cached)
        }
        Work::Routing { unit, file_id } => {
            let bytes = unit.end_byte - unit.start_byte;
            let request = routing::request_for(config.model, &unit);
            drop(unit);
            let (result, cached) =
                evaluate_routing(evaluator, request, config.cache_namespace, cache).await;
            Outcome::Routing {
                file_id,
                bytes,
                result,
                cached,
            }
        }
    }
}

async fn evaluate_routing<E>(
    evaluator: Option<&E>,
    request: Request,
    namespace: &str,
    cache: &Cache,
) -> (Result<[f64; 4], String>, bool)
where
    E: Evaluator,
{
    let keys = routing::IDS.map(|id| cache_key(&format!("{namespace}\0{id}"), &request));
    let mut scores = [0.0; 4];
    let mut all_cached = true;
    for (index, key) in keys.iter().enumerate() {
        match cache.get(key).await {
            Ok(Some(score)) => scores[index] = score,
            Ok(None) => all_cached = false,
            Err(error) => return (Err(error), false),
        }
    }
    if all_cached {
        return (Ok(scores), true);
    }
    let result = async {
        let evaluator = evaluator.ok_or("no evaluator configured")?;
        let answers = evaluator.evaluate(request).await?;
        for (index, id) in routing::IDS.into_iter().enumerate() {
            scores[index] = probability(&answers, id)?;
        }
        for (key, score) in keys.iter().zip(scores) {
            cache.insert(key, score).await?;
        }
        Ok(scores)
    }
    .await;
    (result, false)
}

async fn evaluate_unit<E>(
    evaluator: Option<&E>,
    unit: Unit,
    config: EvaluationConfig<'_>,
    cache: &Cache,
) -> (Unit, Result<f64, String>, bool)
where
    E: Evaluator,
{
    let request = request_for(config.mode, config.query, config.model, &unit);
    let key = cache_key(config.cache_namespace, &request);
    match cache.get(&key).await {
        Ok(Some(value)) => return (unit, Ok(value), true),
        Ok(None) => {}
        Err(error) => return (unit, Err(error), false),
    }
    let Some(evaluator) = evaluator else {
        return (unit, Err("no evaluator configured".into()), false);
    };
    let result = async {
        let value = probability(&evaluator.evaluate(request).await?, QUESTION_ID)?;
        cache.insert(&key, value).await?;
        Ok(value)
    }
    .await;
    (unit, result, false)
}

fn cache_key(namespace: &str, request: &Request) -> String {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut hash = HashWriter(Sha256::new());
    hash.0.update(namespace.as_bytes());
    hash.0.update([0]);
    serde_json::to_writer(&mut hash, request).expect("requests are serializable");
    hash.0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::scanner::{ScanConfig, UnitMode};
    use crate::tests::TestDirectory;

    #[derive(Debug, Default)]
    struct FakeEvaluator {
        calls: AtomicUsize,
        active: AtomicUsize,
        maximum: AtomicUsize,
    }

    struct Active<'a>(&'a AtomicUsize);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Evaluator for FakeEvaluator {
        async fn evaluate(&self, request: Request) -> Result<BTreeMap<String, Answer>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _guard = Active(&self.active);
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await;
            if request.questions.contains_key(routing::IDS[0]) {
                let passage = request.state["passage"].as_str().unwrap();
                if passage.contains("SLOW_ROUTE") {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                if passage.contains("ROUTE_FAIL") {
                    return Err("fixture routing failure".into());
                }
                let winner = if passage.contains("fixture_css") {
                    2
                } else if passage.contains("fixture_rust") {
                    1
                } else {
                    3
                };
                return Ok(routing::IDS
                    .into_iter()
                    .enumerate()
                    .map(|(index, id)| {
                        (
                            id.into(),
                            Answer::Noul {
                                noul: if index == winner { 0.99 } else { 0.01 },
                            },
                        )
                    })
                    .collect());
            }
            if request.state["target"]
                .as_str()
                .unwrap()
                .contains("failure")
            {
                Err("fixture failure".into())
            } else {
                Ok(BTreeMap::from([(
                    QUESTION_ID.into(),
                    Answer::Noul { noul: 0.9 },
                )]))
            }
        }
    }

    fn scanner(directory: &TestDirectory) -> Scanner {
        Scanner::new(
            std::slice::from_ref(&directory.0),
            ScanConfig {
                max_file_bytes: 1_000_000,
                max_unit_bytes: 128,
                window_lines: 10,
                overlap_lines: 0,
                unit_mode: UnitMode::Paragraph,
            },
            None,
        )
    }

    fn config() -> EvaluationConfig<'static> {
        EvaluationConfig {
            mode: Mode::Grep,
            query: "condition",
            model: "fixture-model",
            concurrency: 3,
            cache_namespace: "http://fixture",
            dry_run: false,
            progress: None,
        }
    }

    #[tokio::test]
    async fn lazy_pipeline_bounds_admission_and_stops_when_dropped() {
        let directory = TestDirectory::new();
        for index in 0..50 {
            std::fs::write(directory.0.join(format!("{index}.txt")), "match").unwrap();
        }
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let (tx, rx) = tokio::sync::watch::channel(Default::default());
        let mut config = config();
        config.progress = Some(&tx);
        let events = pipeline(
            Some(&evaluator),
            scanner(&directory),
            config,
            &cache,
            Cancellation::default(),
        );
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(rx.borrow().units_total, 0);
        {
            let mut events = std::pin::pin!(events);
            assert!(matches!(events.next().await, Some(PipelineEvent::Match(_))));
            let coverage = rx.borrow().clone();
            assert!(
                coverage.units_total <= 3,
                "source read ahead of the shared bound: {coverage:?}"
            );
            assert!(!coverage.discovery_complete);
            assert!(evaluator.maximum.load(Ordering::SeqCst) <= 3);
        }
        let calls = evaluator.calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), calls);
        assert_eq!(evaluator.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancellation_reports_known_work_without_claiming_unseen_input() {
        let directory = TestDirectory::new();
        for index in 0..50 {
            std::fs::write(directory.0.join(format!("{index}.txt")), "match").unwrap();
        }
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let cancellation = Cancellation::default();
        let events = pipeline(
            Some(&evaluator),
            scanner(&directory),
            config(),
            &cache,
            cancellation.clone(),
        );
        let mut events = std::pin::pin!(events);
        assert!(matches!(events.next().await, Some(PipelineEvent::Match(_))));
        cancellation.cancel();
        let Some(PipelineEvent::Finished(coverage)) = events.next().await else {
            panic!("missing cancellation summary")
        };
        assert!(!coverage.discovery_complete);
        assert!(!coverage.complete());
        assert_eq!(
            coverage.units_succeeded + coverage.units_cancelled,
            coverage.units_total
        );
        assert!(coverage.units_total <= 3);
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn errors_and_previews_are_streamed_and_counted() {
        let directory = TestDirectory::new();
        std::fs::write(directory.0.join("one.txt"), "match\n\nfailure").unwrap();
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let events = pipeline(
            Some(&evaluator),
            scanner(&directory),
            config(),
            &cache,
            Cancellation::default(),
        );
        let mut events = std::pin::pin!(events);
        let mut failures = 0;
        let mut matches = 0;
        while let Some(event) = events.next().await {
            match event {
                PipelineEvent::Match(_) => matches += 1,
                PipelineEvent::Failure(_) => failures += 1,
                PipelineEvent::Finished(coverage) => {
                    assert!(coverage.discovery_complete);
                    assert!(!coverage.complete());
                    assert_eq!(coverage.units_failed, 1);
                    assert_eq!(coverage.files_scanned, 1);
                }
                _ => {}
            }
        }
        assert_eq!((matches, failures), (1, 1));
        let mut preview_config = config();
        preview_config.dry_run = true;
        let previews = pipeline::<FakeEvaluator>(
            None,
            scanner(&directory),
            preview_config,
            &cache,
            Cancellation::default(),
        );
        let mut previews = std::pin::pin!(previews);
        let mut count = 0;
        while let Some(event) = previews.next().await {
            match event {
                PipelineEvent::Request(_) => count += 1,
                PipelineEvent::Finished(coverage) => {
                    assert!(coverage.complete());
                    assert_eq!(coverage.units_previewed, 2);
                    assert_eq!(coverage.evaluations_scheduled, 0);
                }
                _ => panic!("unexpected preview event"),
            }
        }
        assert_eq!(count, 2);
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 2);
    }
    fn auto_scanner(directory: &TestDirectory) -> Scanner {
        Scanner::new(
            std::slice::from_ref(&directory.0),
            ScanConfig {
                max_file_bytes: 1_000_000,
                max_unit_bytes: 128,
                window_lines: 20,
                overlap_lines: 0,
                unit_mode: UnitMode::Auto,
            },
            None,
        )
    }

    #[tokio::test]
    async fn whole_file_routing_is_bounded_and_reuses_query_independent_cache() {
        let directory = TestDirectory::new();
        // The misleading extension is deliberately ignored, with many routing windows.
        std::fs::write(
            directory.0.join("style.rs"),
            "fixture_css { color: red; }\n".repeat(80),
        )
        .unwrap();
        let cached = TestDirectory::new();
        let cache = Cache::new(Some(cached.0.clone())).unwrap();
        let evaluator = FakeEvaluator::default();
        for run in 0..2 {
            let mut cfg = config();
            cfg.query = if run == 0 { "first" } else { "second" };
            let events = pipeline(
                Some(&evaluator),
                auto_scanner(&directory),
                cfg,
                &cache,
                Cancellation::default(),
            );
            let mut events = std::pin::pin!(events);
            let mut routed = false;
            let mut matches = 0;
            while let Some(event) = events.next().await {
                match event {
                    PipelineEvent::Routed(decision) => {
                        assert_eq!(decision.segmenter, Some("css"));
                        assert!(decision.windows > 10);
                        routed = true;
                    }
                    PipelineEvent::Match(item) => {
                        assert!(routed);
                        assert_eq!(item.unit.kind, "CSS");
                        matches += 1;
                    }
                    PipelineEvent::Finished(coverage) => {
                        assert!(coverage.complete());
                        assert_eq!(coverage.files_routed, 1);
                        assert_eq!(coverage.files_scanned, 1);
                        assert_eq!(coverage.routing_succeeded, coverage.routing_windows);
                        assert_eq!(coverage.cache_hits, 0, "search query changed");
                        if run == 1 {
                            assert_eq!(coverage.routing_cache_hits, coverage.routing_windows);
                        }
                    }
                    event => panic!("unexpected {event:?}"),
                }
            }
            assert!(matches > 10);
        }
        assert!(evaluator.maximum.load(Ordering::SeqCst) <= config().concurrency);
        assert!(evaluator.maximum.load(Ordering::SeqCst) > 1);
    }

    #[tokio::test]
    async fn routing_failure_never_silently_becomes_a_fallback_or_match() {
        let directory = TestDirectory::new();
        std::fs::write(directory.0.join("one.txt"), "ROUTE_FAIL").unwrap();
        std::fs::write(
            directory.0.join("two.txt"),
            "Good sentence. Another sentence.",
        )
        .unwrap();
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let events = pipeline(
            Some(&evaluator),
            auto_scanner(&directory),
            config(),
            &cache,
            Cancellation::default(),
        );
        let mut events = std::pin::pin!(events);
        let mut issues = 0;
        let mut good = 0;
        while let Some(event) = events.next().await {
            match event {
                PipelineEvent::ScanIssue(issue) => {
                    assert!(issue.message.contains("routing failed"));
                    issues += 1;
                }
                PipelineEvent::Routed(decision) => assert!(decision.path.ends_with("two.txt")),
                PipelineEvent::Match(item) => {
                    assert!(item.unit.path.ends_with("two.txt"));
                    good += 1;
                }
                PipelineEvent::Finished(coverage) => {
                    assert!(!coverage.complete());
                    assert_eq!(coverage.routing_failed, 1);
                    assert_eq!(coverage.files_scanned, 1);
                    assert_eq!(coverage.scan_issues, 1);
                }
                event => panic!("unexpected {event:?}"),
            }
        }
        assert_eq!(issues, 1);
        assert_eq!(good, 2);
    }

    #[tokio::test]
    async fn auto_preview_shows_routing_without_inventing_model_decisions() {
        let directory = TestDirectory::new();
        std::fs::write(directory.0.join("one.txt"), "Some text.").unwrap();
        let cache = Cache::default();
        let mut cfg = config();
        cfg.dry_run = true;
        let events = pipeline::<FakeEvaluator>(
            None,
            auto_scanner(&directory),
            cfg,
            &cache,
            Cancellation::default(),
        );
        let mut events = std::pin::pin!(events);
        let mut requests = 0;
        while let Some(event) = events.next().await {
            match event {
                PipelineEvent::Request(request) => {
                    assert_eq!(request.questions.len(), 4);
                    assert!(!request.state.as_object().unwrap().contains_key("query"));
                    requests += 1;
                }
                PipelineEvent::Routed(decision) => {
                    assert!(decision.segmenter.is_none());
                    assert!(decision.scores.is_none());
                }
                PipelineEvent::Finished(coverage) => {
                    assert!(coverage.complete());
                    assert_eq!(coverage.routing_previewed, 1);
                    assert_eq!(coverage.units_previewed, 0);
                    assert_eq!(coverage.files_routed, 0);
                }
                event => panic!("unexpected {event:?}"),
            }
        }
        assert_eq!(requests, 1);
    }

    #[tokio::test]
    async fn cancellation_during_routing_accounts_for_pending_windows() {
        let directory = TestDirectory::new();
        std::fs::write(directory.0.join("one.txt"), "fixture_rust;\n".repeat(1000)).unwrap();
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let cancellation = Cancellation::default();
        let (tx, rx) = tokio::sync::watch::channel(EvaluationCoverage::default());
        let mut cfg = config();
        cfg.progress = Some(&tx);
        let events = pipeline(
            Some(&evaluator),
            auto_scanner(&directory),
            cfg,
            &cache,
            cancellation.clone(),
        );
        let mut events = std::pin::pin!(events);
        let cancel = async {
            let mut rx = rx;
            while rx.borrow_and_update().routing_windows < cfg.concurrency {
                rx.changed().await.unwrap();
            }
            cancellation.cancel();
        };
        let (event, ()) = tokio::join!(events.next(), cancel);
        let Some(PipelineEvent::Finished(coverage)) = event else {
            panic!("expected cancellation");
        };
        assert!(!coverage.complete());
        assert_eq!(coverage.routing_cancelled, cfg.concurrency);
        assert_eq!(coverage.units_total, 0);
        assert!(coverage.routing_windows <= cfg.concurrency);
        assert!(events.next().await.is_none());
    }
    #[test]
    fn noul_answers_must_be_present_well_typed_and_valid() {
        let mut answers = BTreeMap::new();
        assert!(
            probability(&answers, "rust")
                .unwrap_err()
                .contains("omitted")
        );
        answers.insert(
            "rust".into(),
            Answer::Choice {
                choice: "yes".into(),
                probabilities: BTreeMap::new(),
                confidence: 1.0,
            },
        );
        assert!(
            probability(&answers, "rust")
                .unwrap_err()
                .contains("expected noul")
        );
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            answers.insert("rust".into(), Answer::Noul { noul: value });
            assert!(probability(&answers, "rust").is_err());
        }
    }
    #[tokio::test]
    async fn independent_files_route_concurrently_and_fast_files_search_before_slow_routes_finish()
    {
        let directory = TestDirectory::new();
        let slow = directory.0.join("slow.txt");
        let fast = directory.0.join("fast.txt");
        std::fs::write(&slow, "SLOW_ROUTE sentence.").unwrap();
        std::fs::write(&fast, "Fast sentence.").unwrap();
        let scanner = Scanner::new(
            &[slow.clone(), fast.clone()],
            ScanConfig {
                max_file_bytes: 10000,
                max_unit_bytes: 1000,
                window_lines: 80,
                overlap_lines: 0,
                unit_mode: UnitMode::Auto,
            },
            None,
        );
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let (tx, rx) = tokio::sync::watch::channel(EvaluationCoverage::default());
        let mut cfg = config();
        cfg.progress = Some(&tx);
        let events = pipeline(
            Some(&evaluator),
            scanner,
            cfg,
            &cache,
            Cancellation::default(),
        );
        let mut events = std::pin::pin!(events);
        let mut slow_routed = false;
        let mut fast_matched = false;
        while let Some(event) = events.next().await {
            match event {
                PipelineEvent::Routed(decision) if decision.path == slow => slow_routed = true,
                PipelineEvent::Match(item) if item.unit.path == fast => {
                    assert!(!slow_routed, "fast file waited behind slow file routing");
                    fast_matched = true;
                }
                PipelineEvent::Finished(coverage) => assert!(coverage.complete()),
                _ => {}
            }
            assert!(rx.borrow().files_discovered - rx.borrow().files_scanned <= cfg.concurrency);
        }
        assert!(fast_matched && slow_routed);
        assert!(evaluator.maximum.load(Ordering::SeqCst) >= 2);
        assert!(evaluator.maximum.load(Ordering::SeqCst) <= cfg.concurrency);
    }

    #[tokio::test]
    async fn routing_with_one_shared_slot_makes_progress_without_deadlock() {
        let directory = TestDirectory::new();
        std::fs::write(directory.0.join("one.txt"), "One sentence.").unwrap();
        std::fs::write(directory.0.join("two.txt"), "Two sentences. Another.").unwrap();
        let evaluator = FakeEvaluator::default();
        let cache = Cache::default();
        let mut cfg = config();
        cfg.concurrency = 1;
        let events = pipeline(
            Some(&evaluator),
            auto_scanner(&directory),
            cfg,
            &cache,
            Cancellation::default(),
        );
        let mut events = std::pin::pin!(events);
        let drain = async {
            let mut matched = 0;
            while let Some(event) = events.next().await {
                match event {
                    PipelineEvent::Match(_) => matched += 1,
                    PipelineEvent::Finished(coverage) => {
                        assert!(coverage.complete());
                        assert_eq!(coverage.files_scanned, 2);
                    }
                    _ => {}
                }
            }
            assert_eq!(matched, 3);
        };
        tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .unwrap();
        assert_eq!(evaluator.maximum.load(Ordering::SeqCst), 1);
    }
}
