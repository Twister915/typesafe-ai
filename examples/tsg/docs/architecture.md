# How TSG works

[TSG](../README.md) · [CLI reference](cli.md) · [How it works](architecture.md)

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

The four implementations are in [`segmenters.rs`](../segmenters.rs), each at most **30
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

## Verification scope

Unit tests cover routing aggregation, cache reuse across queries, independent request
criteria, missing/wrong/invalid answers, cancellation, bounded admission, Unicode/source
boundaries, snapshot replay after source changes, temporary-file cleanup, concurrent file routing, early results despite a stalled routing
request for another file, and single-slot progress. Loopback
fixtures exercise HTTP behavior; tests never require the live TypeSafe service. Model
routing accuracy and these policy thresholds still need evaluation on a labeled corpus
of real source, prose, mixed documents, and unsupported formats.
