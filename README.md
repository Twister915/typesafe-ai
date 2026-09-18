# TypeSafe for Rust

`typesafe-ai` brings [TypeSafe's](https://typesafe.ai) System One evaluation API into
Rust as small, typed judgments that fit inside ordinary application code.

Send the state your application already has, ask focused questions, and combine the
answers with your own rules. A request can:

- route a support case to one of your teams;
- classify content against a fixed set of labels;
- score an item along an ordered rubric;
- judge whether a condition is true and return its probability.

## Why this crate

- **Typed results.** Noul, Choice, and Score responses deserialize into Rust enums with
  probabilities, distributions, confidence, and usage data.
- **Sync or async.** Use the default asynchronous reqwest client, or disable default
  features and choose the blocking ureq client without pulling in Tokio.
- **One state, several judgments.** Batch independent questions over shared text or
  structured JSON.
- **Visible retries when you need them.** Use `evaluate` for a simple result, or observe
  each failure and planned retry through a lazy iterator or stream.
- **Errors you can inspect.** Status, headers, request IDs, original response bytes, and
  best-effort structured API details remain available without exposing credentials in display
  text. Use `ApiErrorDetails::try_from(error)` for recognized details or `Error::body()` for the
  raw body.

## A small example

```rust
use typesafe_ai::{Question, Request, ReqwestClient, ReqwestError};

async fn refund_probability(
    client: &ReqwestClient,
    message: &str,
) -> Result<Option<f64>, ReqwestError> {
    let request = Request::new(message).with_question(
        "requests_refund",
        Question::noul("Is the customer asking for a refund?"),
    );

    let response = client.evaluate(&request).await?;
    Ok(response
        .answer("requests_refund")
        .and_then(|answer| answer.noul()))
}
```

The model supplies a bounded judgment; your code keeps control of thresholds, routing,
side effects, and the larger workflow.

See [Getting started](docs/getting-started.md) for installation, feature selection,
all three primitives, and runnable examples.

## Documentation

- [Getting started](docs/getting-started.md) — install the crate, choose a client, and
  build requests.
- [Configuration and errors](docs/configuration.md) — timeouts, retries, custom transports,
  response bytes, and typed errors.
- [Progress events](docs/progress-events.md) — observe retries with a blocking iterator or
  asynchronous stream.
- [Pull request questions](examples/pr_questions/README.md) — a clap CLI that applies
  `.ts_rules/*.json` to PR metadata and diffs, with a GitHub Actions setup guide.
- [Semantic search with `tsg`](examples/tsg/README.md) — a Tokio and clap CLI that
  finds text passages relevant to a topic, task, or question, or filters them by a predicate,
  with bounded concurrent evaluation and original source locations.
- Rust API documentation — build the current reference locally with
  `cargo doc --open --all-features`.
- [TypeSafe API reference](https://docs.typesafe.ai/api) and
  [primitive guide](https://docs.typesafe.ai/primitives) — service behavior and question
  design.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
