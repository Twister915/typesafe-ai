# Pull request questions

`pr_questions` is a standalone clap CLI example using the blocking TypeSafe client.
It reads question maps from `$PWD/.ts_rules/` and evaluates them together against a
PR's title, description, and diff. It has no GitHub API or Actions dependency.
The only required environment variable for evaluation is `TYPESAFE_API_KEY`.

## Build and run

From this repository, build the executable:

```sh
cargo build --locked --release --no-default-features \
  --features ureq,rustls-tls --example pr_questions
```

Then run it from the repository you want to inspect. For example, from this repository
after it has a Git history and an `origin/main` branch:

```sh
git diff --no-ext-diff --no-textconv --no-color origin/main...HEAD -- > /tmp/pr.diff
./target/release/examples/pr_questions \
  --title 'Add PR question evaluation' \
  --description 'A portable CLI that applies repository questions to a diff.' \
  --diff /tmp/pr.diff > /tmp/pr-answers.json
```

Supply `TYPESAFE_API_KEY` through your shell's secret management or CI secret injection.
The key has no CLI flag and is never included in request previews or diagnostic logs.
All other inputs are CLI flags; there is no tool configuration file beyond the questions.

| Flag | Behavior |
| --- | --- |
| `--title TEXT` | Required PR title. |
| `--description TEXT` | PR description; empty when omitted. |
| `--description-file PATH` | Read a UTF-8 description; conflicts with `--description`. |
| `--diff PATH` | Read a UTF-8 diff; defaults to `-` for stdin. |
| `--rules-dir PATH` | Question directory; defaults to `.ts_rules`. |
| `--model NAME` | Model identifier; defaults to `jev-latest`. |
| `--dry-run` | Validate and print the request without a key or network access. |

Relative paths resolve against the process's current working directory, not the
executable or Cargo manifest directory. Generate the diff before invoking the CLI so
a failed Git command cannot accidentally become an empty diff. The CLI accepts an
empty diff, which is useful for metadata-only evaluations. Use `--help` for usage.

To inspect the request before sending it:

```sh
cargo run --locked --no-default-features --features ureq,rustls-tls \
  --example pr_questions -- \
  --title 'Inspect this change' --diff /tmp/pr.diff --dry-run
```

## Question files and state

Every regular, immediate child file with the lowercase `.json` extension contains a
complete `questions` map. There is no outer `questions` field and no filename-derived
question ID. Files are loaded in sorted path order and merged by ID; subdirectories,
symlinks, and other file extensions are ignored. For example, `.ts_rules/review.json`:

```json
{
  "public_api_break": {
    "type": "noul",
    "instructions": "Does `diff` introduce a source-incompatible change to the existing public Rust API? Use `title` and `description` for context.",
    "criteria": {
      "true": "Existing downstream source code would need changes to compile.",
      "false": "Existing downstream source code remains compatible."
    }
  }
}
```

Noul, Choice, and Score questions use the library's existing types, including structured
instructions and criteria. See the [API reference](https://docs.typesafe.ai/api) and
[structured question guide](https://docs.typesafe.ai/primitives/advanced). IDs must be
unique both within each file and across files. A missing directory, no questions,
invalid JSON, invalid question shape, or duplicate ID fails before evaluation.

The request's `state` is a JSON object with three string fields:

```json
{
  "title": "The supplied PR title",
  "description": "The supplied PR description",
  "diff": "The supplied git diff output"
}
```

The entire supplied state is sent to TypeSafe. The CLI does not fetch Git history,
execute a shell, truncate large diffs, or split questions across requests. Git's usual
text diff reports binary changes without showing binary contents, so these questions
cannot evaluate the contents of changed binary files.

The repository includes initial questions about public API compatibility and retries
after ambiguous failures in `.ts_rules/`. They are review signals for dogfooding;
deterministic checks such as compilation and dependency isolation remain CI's job.

## Output, errors, and retries

Normal execution writes the original successful API response JSON to stdout, followed
by a newline. This retains probabilities, decimal spelling, any optional usage, and
additional response fields. Diagnostics go to stderr without printing PR state or the
API key. `--dry-run` instead writes the validated request JSON to stdout.

Exit status is `0` for successful evaluation or preview, `1` for input, API, or output
errors, and `2` for clap argument errors. Answers do not automatically fail the command:
an arbitrary Noul, Choice, or Score question has no universal pass condition. Consumers
can inspect the JSON and apply their own policy after evaluating thresholds on real PRs.

The shared client's defaults apply: a 60-second timeout per attempt and at most two
retries after the initial attempt, only for HTTP 429 or 529. Backoff starts at 250 ms,
doubles to an 8-second cap, and honors longer valid server delays up to 60 seconds.
A longer requested delay ends the evaluation with the last API error. Transport errors,
timeouts, decoding errors, and other HTTP errors are not retried. The example adds no
retry loop.

## GitHub Actions

Add `TYPESAFE_API_KEY` as a repository Actions secret. Once the example and rules are
merged into the base branch, save the following as `.github/workflows/pr-questions.yml`
to dogfood this repository. It runs on same-repository PRs; fork and Dependabot PRs are
skipped because ordinary `pull_request` workflows do not receive those secrets.

The checkout and build use the PR's **base revision**, including its rule files. The
head commit is used only to generate a diff and is never checked out or executed. Edits
to the CLI or rules therefore take effect after merging. PR text is passed through
environment variables and files rather than interpolated into shell source. See GitHub's
[PR event](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request)
and [script injection](https://docs.github.com/en/actions/reference/security/secure-use#script-injections) guidance.

```yaml
name: PR questions

on:
  pull_request:
    types: [opened, synchronize, reopened, edited]

permissions:
  contents: read

jobs:
  evaluate:
    if: >-
      github.event.pull_request.head.repo.full_name == github.repository &&
      github.actor != 'dependabot[bot]'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event.pull_request.base.sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Install nightly Rust
        run: rustup toolchain install nightly --profile minimal
      - name: Build trusted example
        run: >-
          cargo build --locked --release --no-default-features
          --features ureq,rustls-tls --example pr_questions
      - name: Prepare PR inputs
        env:
          BASE_SHA: ${{ github.event.pull_request.base.sha }}
          HEAD_SHA: ${{ github.event.pull_request.head.sha }}
        run: |
          git cat-file -e "$HEAD_SHA^{commit}"
          git diff --no-ext-diff --no-textconv --no-color "$BASE_SHA...$HEAD_SHA" -- > "$RUNNER_TEMP/pr.diff"
          jq -j '.pull_request.body // ""' "$GITHUB_EVENT_PATH" > "$RUNNER_TEMP/pr-description.txt"
      - name: Evaluate questions
        env:
          TYPESAFE_API_KEY: ${{ secrets.TYPESAFE_API_KEY }}
          PR_TITLE: ${{ github.event.pull_request.title }}
        run: |
          ./target/release/examples/pr_questions \
            --title "$PR_TITLE" \
            --description-file "$RUNNER_TEMP/pr-description.txt" \
            --diff "$RUNNER_TEMP/pr.diff" > "$RUNNER_TEMP/pr-answers.json"
          jq . "$RUNNER_TEMP/pr-answers.json"
```

`fetch-depth: 0` makes same-repository branch history available; the explicit commit
check fails if the event's head is no longer available, rather than evaluating a newer
commit. The three-dot diff compares the merge base with the event's head, matching PR
change scope. Results appear in the job log; the workflow does not post comments or
fail a PR based on a model answer.

For another repository, install the example from a trusted local checkout of this
package, then invoke `pr_questions` from that repository's base checkout:

```sh
cargo install --locked --path /path/to/typesafe-ai \
  --no-default-features --features ureq,rustls-tls --example pr_questions
```

## Packaging

[Cargo example targets](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#examples)
are real executables that can use development dependencies and can be explicitly
installed with `cargo install --example`. Keeping this under `examples/pr_questions/`
ships its source with the library while keeping clap and logging setup out of the
library's normal dependency graph. There is no nested Cargo package or workspace.

If the CLI later needs its own versioning or release artifacts, a separate package is
a reasonable next step. For a runnable SDK example and initial dogfooding, this target
keeps packaging simple. Its unit tests run with `cargo test --all-features`.
