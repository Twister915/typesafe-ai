# tsg

**Find the code you mean. Find the passage you need.**

Search your code and documents in plain English. `tsg` uses TypeSafe to find
passages by meaning, then takes you straight to the original text—with file
paths, line numbers, and enough context to keep reading.

```sh
tsg find 'Where is retry behavior configured?' src/ docs/
tsg grep 'Does this code silently discard an error?' src/
```

You know what you're looking for. You shouldn't have to guess what someone
called it.

## Ask a question. Get the source.

Explore an unfamiliar codebase, trace a behavior, or pull relevant requirements
from a folder of documents. Every result is a passage from your files that you
can inspect for yourself.

- **Search across wording.** Describe a behavior, topic, or requirement in your own words.
- **Go straight to the evidence.** Read line-numbered source previews and see exactly where each match came from.
- **Start with the files you have.** Search local UTF-8 code, Markdown, and text without building an index. Git ignore rules are respected.
- **Take results into your workflow.** Read them in the terminal or use `--json` for scripts and other tools.

## Two ways to search

Use **`find`** when you're investigating a question. It ranks relevant passages
so you can start with the most useful evidence.

```sh
tsg find 'How is overload handled?' src/ --top 8
tsg find 'requirements relevant to operating a food truck' examples/tsg/demo/
```

Use **`grep`** when you have a condition in mind. It returns passages judged to
meet that condition, so multiple matches can stand on their own.

```sh
tsg grep 'Does this code retry a failed operation?' src/
tsg grep 'Does this passage state a permit requirement?' examples/tsg/demo/
```

The [bundled demo](demo/) contains fictional rules for trying document searches.

## Try it

From the root of this repository, install with the project's nightly Rust toolchain:

```sh
cargo install --locked --path . --example tsg
export TYPESAFE_API_KEY='your-api-key'
tsg find 'Where is retry behavior configured?' src/ docs/
```

Searches send source passages and nearby context to the TypeSafe API and require
a TypeSafe API key. To inspect a request locally without a key or network access:

```sh
tsg grep 'Does this retry?' src/ --unit rust --dry-run --json
```

## Keep reading where the answer lives

`tsg` brings you back to the source: ranked evidence for an investigation, or
matching passages for a targeted search. Semantic judgments can miss things;
results are starting points for review. If a file or evaluation fails, the scan
reports incomplete coverage.

Ready to tune your search? See the [CLI reference](docs/cli.md) for thresholds,
output, caching, and controls, or [how it works](docs/architecture.md) for routing,
passage boundaries, and coverage details.
