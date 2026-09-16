# TSG CLI reference

[TSG](../README.md) · [CLI reference](cli.md) · [How it works](architecture.md)

All commands below run from the repository root.

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
