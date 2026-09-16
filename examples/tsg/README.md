# `tsg`: source-preserving semantic search

`tsg` is a substantial Tokio and clap example built on the async TypeSafe client. It
streams a selected local scope, routes each file to a small segmenter using plain-English
Noul criteria, then asks one independent Noul search question about every resulting passage.
It keeps exact source locations without loading the corpus or a whole file into memory.

The two subcommands answer different questions:

- `tsg grep CONDITION PATH...` returns each passage whose own behavior or meaning
  satisfies a condition. Any number of passages can match.
- `tsg find QUERY PATH...` ranks passages by whether they contain evidence useful for
  investigating a question, topic, or task.

`grep` deliberately does not put all passages into one Choice question. Choice options
compete for probability mass, so adding another good match would lower the probabilities
of existing matches. One Noul per passage preserves all-match semantics. `find` also uses
independent Nouls and keeps a bounded top-K ranking, so its results remain comparable and
its coverage is easy to explain.

## Run it

Install the example as the `tsg` command from this checkout:

```console
cargo install --locked --path . --example tsg
tsg find 'requirements relevant to operating a food truck' path/to/laws/
tsg grep 'Does this passage impose a permit requirement?' path/to/laws/
```

Evaluation reads `TYPESAFE_API_KEY` from the environment. You can also run directly
from the crate root:

```console
export TYPESAFE_API_KEY=...
cargo run --example tsg -- grep \
  'Does this code retry a failed operation?' src/
cargo run --example tsg -- find \
  'Where is retry behavior configured?' src/ docs/
```

The bundled fictional Markdown laws are a small document-oriented example:

```console
cargo run --example tsg -- find \
  'requirements relevant to operating a food truck' examples/tsg/demo/
cargo run --example tsg -- grep \
  'Does this passage state a permit requirement?' examples/tsg/demo/
```

`find` treats definitions, constraints, exceptions, dependencies, and references as
potentially useful evidence. It returns the raw passages for review; it does not resolve
cross-references or make a legal conclusion.

`grep` defaults to a `0.7` threshold and returns every match. `find` defaults to a `0.5`
threshold and the top 20 results. These are demo policies, not calibrated universal
cutoffs. Override them for your corpus; pass `--threshold 0` to inspect every ranked
candidate:

```console
cargo run --example tsg -- grep \
  'Does this discard an error?' . --threshold 0.82 --top 100
cargo run --example tsg -- find \
  'How is overload handled?' . --threshold 0.45 --top 8 --json
```

Every discovered passage is evaluated. `--threshold` filters each result as it arrives;
`--top` limits displayed results without pruning the input or creating a retrieval shortlist.

Use `--dry-run` to inspect API requests without an API key or network access. With
`--unit auto`, this previews routing requests only: there is no model response with which
to select a segmenter. Pass an explicit `--unit rust`, `javascript`, `css`, `prose`,
`section`, `paragraph`, or `window` to preview search requests for that segmenter:

```console
cargo run --example tsg -- grep \
  'Does this retry?' src/ --unit rust --dry-run --json
```

Every question explicitly refers to the named `target`, `query`, `headings`, and
`context` fields in state. Question-map IDs are response correlation keys and are not
used by the model for inference.

## Passage construction and coverage

The scanner follows `.gitignore`, `.ignore`, global Git excludes, and hidden-file filters
through the `ignore` crate. It does not follow symbolic links. Discovery, file reads,
routing, segmentation, cache lookup, evaluation, and output form one bounded pipeline.
Every file is treated as UTF-8 text. No programming-language or data-format parser is used.

### Content-based file routing

`--unit auto` makes one segmenter decision for the file from **all its contents**, rather
than choosing by extension or looking only at its opening lines:

1. Read consecutive nonoverlapping byte-bounded windows, teeing the original bytes into
   a private temporary snapshot for that file. Boundary whitespace may be omitted from model input;
   the snapshot retains every original byte. Each window has at most `--max-unit-bytes`
   target bytes plus bounded preceding context. Routing does not use `--window-lines`.
2. Ask four independent Nouls together over each window: is its primary authored content
   JavaScript/TypeScript, Rust, a CSS-family stylesheet, or natural-language prose?
   The criteria distinguish actual source from documents discussing or quoting source,
   comments belonging to code, and lookalikes such as JSON or Cargo.toml. They include
   JSX/TSX, CSS/SCSS/Less, prose documentation, and legal rules/definitions/exceptions.
3. Accumulate four byte-weighted mean scores, retaining no window bodies after evaluation.
   Select the highest score only if it is at least `0.70` and exceeds the next score by
   at least `0.15`. Otherwise select generic `window` segmentation. These constants are
   example routing policy, and the means are **not calibrated whole-file probabilities**.
4. After every window has been judged, stream the chosen segmenter over the same snapshot,
   then remove it. A file changed on disk after routing cannot change this replay.

This is a streaming reduction of judgments across a file, not one API request containing
an arbitrarily large file. Small files need one routing request; larger files need one
per window. Routing adds a pass and model latency before that file's search passages can
be emitted. A bounded set of files progresses concurrently: one file can be read while another
awaits routing, and already-routed files stream search passages immediately. Files and
routing windows are not serialized behind earlier model calls. A slow file does not
block other admitted files. A failed routing call, missing answer, or invalid
probability skips that file with an explicit issue and incomplete coverage; it never
masquerades as an uncertain-but-successful fallback.

### Small boundary functions

The four implementations are in [`segmenters.rs`](segmenters.rs), each at most **30
physical lines after rustfmt**, enforced by a test. Shared source reading, byte/line
budgets, exact offsets, and preceding context live in the scanner, not in language parsers.

| Segmenter | Simple boundary rule |
| --- | --- |
| `javascript` | Complete source lines, with shallow continuation handling for multiline expressions and semicolon-free JavaScript/TypeScript. |
| `rust` | Blank lines, semicolon-ended lines, or standalone closing-brace lines. |
| `css` | Closing-rule lines and standalone semicolon-ended at-rules. |
| `prose` | Unicode UAX #29 default sentence boundaries using `unicode-segmentation`. |

The code heuristics do not understand full syntax. They can split strings/comments or
nested expressions poorly; they do not promise statements, functions, or AST nodes.
Minified code still respects the byte cap. Unicode segmentation is a text-boundary
algorithm, not a language parser. Its default rules include sentence breaks at line
separators, so hard-wrapped prose can produce shorter units. Decimal punctuation and
non-Latin sentence terminators follow the Unicode rules, not an ASCII-period shortcut.
An unfinished sentence is retained across input reads until a stable boundary, EOF, or
a hard limit; source text is never normalized or rewritten.

Explicit `--unit` values bypass model routing entirely. Legacy `section` (handwritten
Markdown ATX headings with bounded heading ancestry and fence handling), `paragraph`
(blank-line spans), and `window` (budget-limited spans) remain available. `auto` routes
prose Markdown to Unicode sentences; use `--unit section` when heading-sized spans are
preferred. These heuristics are query-independent; no file-level relevance decision
prunes search units.

All strategies share byte and line limits. A hard split can occur inside a sentence or
code construct; adjacent forced windows share up to `--overlap-lines` lines when those
fit. Natural boundaries do not overlap. Every constructed passage is independently
searched. Explicit modes may emit earlier units before a later invalid UTF-8/NUL/read
error; auto validates the complete snapshot first. Either failure marks coverage incomplete.

The default limits are:

- 2 MiB per file;
- 24 KiB per target passage;
- 80 lines per fallback window, with 20 overlapping lines;
- 64 concurrent pipeline jobs.

Byte limits still apply to a single minified line. UTF-8 boundaries are preserved when a
large span is split. Nearby context is separately capped at 4 KiB and is only used to
interpret the target.

Configure these with `--max-file-bytes`, `--max-unit-bytes`, `--window-lines`,
`--overlap-lines`, and `--concurrency`. The concurrency budget covers the one active
scanner/read job and all in-flight routing, cache, or API evaluation jobs together, so admitted
pipeline work never exceeds `--concurrency`. Nearby context contains only bounded text
observed before the target. Auto admits at most `--concurrency` file states and disk snapshots, never whole-file
strings in memory. A round-robin reader shares the work budget with routing/search calls;
there is no initial corpus-wide classification pass or unbounded per-file task fan-out.
Each snapshot is bounded by `--max-file-bytes` and removed on normal
completion, errors, stream drop, and handled cancellation.
Oversized files, unreadable files, non-UTF-8 files, NUL-containing files, failed API
calls, missing answers, invalid probabilities, and cancellation are reported as
incomplete coverage. They are never converted into semantic non-matches.

Press Ctrl-C to stop discovery, stop admitting requests, and drop in-flight work.
Coverage then describes only the files and units observed so far and sets
`discovery_complete` to false when input remains unvisited; unseen units cannot be
counted. Dropping an HTTP future cancels local waiting but cannot guarantee that the
server stopped processing a request. The client retries HTTP 429 and 529 twice by
default; `--retries` changes that count. Other failures are not retried.

## Output and exit status

Terminal output uses numbered results, colored source locations, heading breadcrumbs,
probability bars, and line-numbered source previews. Each result shows up to 12 source
lines by default; `--preview-lines N` changes that limit and `--full` shows the entire
passage. Omitted lines are counted explicitly. Preview limits affect display only, not
the text evaluated by the model. These probabilities describe the requested judgment;
they are not a separate measure of model confidence. Scan issues and evaluation failures
are printed when they occur, and every output event is flushed before more pipeline work
is admitted so a slow consumer supplies backpressure.

`grep` emits threshold matches in evaluation-completion order. `--top` caps how many are
displayed but does not stop the scan, and the final summary reports how many qualifying
matches were omitted. `find` must see every probability to identify the global top K, so
it emits its ranked results after discovery and evaluation finish. It keeps O(K) ranking
metadata in memory and spools retained source payloads to temporary files, then reads and
emits one result at a time.

`--color auto` enables colors on a terminal unless `NO_COLOR` is nonempty or `TERM=dumb`.
`--color always` explicitly enables colors, even in a pipe; `--color never` disables
them. While evaluating, an interactive terminal shows completed passages, cache hits,
failures, and elapsed time on stderr. Use `--no-progress` to hide this status. Pipes,
JSON output, and dumb terminals never receive animated progress.

Source control characters are escaped in human output so corpus text cannot manipulate
the terminal. `--json` emits newline-delimited JSON (NDJSON), with one flushed record per
event and a `type` of `start`, `routing`, `match`, `scan_issue`, `failure`, `request`, or `summary`.
A `routing` record includes the selected segmenter, named aggregate scores, window count,
judged bytes (excluding trimmed boundary whitespace), and selection reason. Dry runs
report a null segmenter and scores rather than inventing a classification.
There is no final array of matches or problems. Match records retain the complete original
passage and never include color codes, regardless of `--color` or preview settings.

The final `summary` record includes an overall `complete` flag, discovered and scanned files, whether discovery
completed, and counters for total, scored, previewed, failed, cancelled, cached, and
scheduled units. Scheduled evaluations include cache lookups and exclude dry-run previews;
they may hit the cache, involve retries, or be cancelled before an HTTP request is sent.
Routing has separate window, succeeded, failed, cancelled, previewed, cached, and
files-routed counters. The summary distinguishes
the number of threshold matches from the number returned after `--top`.

Exit statuses are:

- `0`: the scan completed and at least one result matched; dry runs also use `0` when
  file discovery completed without issues;
- `1`: the scan completed with no result at the requested threshold, or CLI setup failed;
- `2`: coverage is incomplete because scanning, evaluation, or cancellation failed.

The model can still produce false positives or false negatives in a complete scan.
“Complete” means discovery finished without scan issues and every discovered unit was
scored or previewed without evaluation failure or cancellation, not that the semantic
judgment is infallible or that local context proves whole-program behavior.

Memory use is bounded by the configured concurrency, the passage and context byte limits,
directory traversal state, and O(K) metadata for `find`. Passage bodies and result collections
do not grow in memory with the corpus or number of `grep` matches.
This example scans the selected files for each query and does not maintain a retrieval
index. It reads UTF-8 text, not PDF or other binary formats.

## Cache and reproducibility

Pass `--cache DIR` to store successful probabilities. The cache is sharded into one small
file per SHA-256 content key and is accessed lazily; cache entries are never loaded into
one in-memory map. The key covers the command mode, query, model string, API base URL,
prompt version, path and byte/line identity, headings, target content, and context. API
keys are never stored. Routing keys include the full routing prompt, model, source window,
path, offsets, context, endpoint, and question ID, but exclude the search query, so another
search can reuse routing decisions. Each of the four Noul values uses a small entry; all
four must be present to skip a routing request. Each entry is written through a uniquely created temporary file
in its shard directory and an atomic rename. The cache directory itself is excluded from
corpus discovery. A legacy single-file JSON cache is rejected with instructions to choose
a new cache directory.

An alias such as `jev-latest` can point to a newer model later. A cached entry keyed by
that literal alias does not automatically expire when the alias changes. Delete the cache,
choose a new cache path, or use a versioned model identifier when exact replay matters.

## Useful controls

```text
--model MODEL             TypeSafe model (default: jev-latest)
--threshold P             finite probability from 0 through 1
--top N                   ranked find limit, or completion-order grep output cap
--concurrency N           shared scanner/cache/API work bound (default: 64)
--cache DIR               optional sharded content-and-question cache
--dry-run                 print requests; no API key or network
--json                    newline-delimited events and final summary
--base-url URL            alternate API base URL, useful for loopback fixtures
--timeout-seconds N       per-attempt timeout (default: 60)
--retries N               retries for HTTP 429 and 529 (default: 2)
--unit MODE               auto, javascript, rust, css, prose, section, paragraph, window
--color MODE              auto, always, or never (also accepted before the subcommand)
--preview-lines N         source lines displayed per result (default: 12)
--full                    display complete source passages
--no-progress             disable the interactive evaluation status
```

Run `cargo run --example tsg -- find --help` or `grep --help` for the complete scanner
options.

## Verification scope

Unit tests cover routing aggregation, cache reuse across queries, independent request
criteria, missing/wrong/invalid answers, cancellation, bounded admission, Unicode/source
boundaries, snapshot replay after source changes, temporary-file cleanup, concurrent file routing, early results despite a stalled routing
request for another file, and single-slot progress. Loopback
fixtures exercise HTTP behavior; tests never require the live TypeSafe service. Model
routing accuracy and these policy thresholds still need evaluation on a labeled corpus
of real source, prose, mixed documents, and unsupported formats.
