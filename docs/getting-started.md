# Getting started

Add `typesafe-ai` with the default asynchronous reqwest client:

```toml
[dependencies]
typesafe-ai = "0.1"
```

Applications using `#[tokio::main]` also need Tokio with the runtime and macro features.
For the blocking ureq client without reqwest or Tokio:

```toml
[dependencies.typesafe-ai]
version = "0.1"
default-features = false
features = ["ureq", "rustls-tls"]
```

The crate requires `std`. Its features are:

| Feature | Default | Provides |
| --- | --- | --- |
| `reqwest` | yes | `ReqwestClient`, an asynchronous Tokio client |
| `ureq` | no | `UreqClient`, a blocking client |
| `rustls-tls` | yes | rustls integration for each enabled backend |

The default reqwest client uses rustls with its AWS-LC crypto backend. Disable all
features to use only the request, response, error, event, and client trait types.

## Build a request

A request supplies shared state and questions keyed by IDs chosen by your application.
Questions in one request are evaluated independently. IDs correlate questions with their
answers and do not affect inference.

```rust
use std::collections::BTreeMap;

use serde_json::json;
use typesafe_ai::{Question, Request};

let request = Request::new("Help! My payouts have been failing for three days.")
    .with_question(
        "is_urgent",
        Question::noul("Does the customer convey urgency?"),
    )
    .with_question(
        "department",
        Question::choice(
            "Which team should handle this?",
            BTreeMap::from([
                ("billing".into(), json!("Payments, invoices, and refunds")),
                ("technical".into(), json!("Bugs, outages, and integrations")),
                ("sales".into(), json!("Pricing, upgrades, and new accounts")),
            ]),
        ),
    )
    .with_question(
        "frustration",
        Question::score(
            "How frustrated is the customer?",
            vec![json!("Calm"), json!("Frustrated"), json!("Very angry")],
        ),
    );
```

`Request::new` selects `jev-latest`. State accepts a string, object, or array.
Instructions and criteria descriptions may be strings or structured JSON.
Applications using the `json!` macro also need `serde_json = "1"`.

Noul returns the probability of yes from `0.0` to `1.0`. Choice returns the selected
option, every option's probability, and distribution-derived confidence. Score returns a
probability-weighted position across ordered levels, its distribution and legend, and
distribution-derived confidence. Confidence measures concentration in a distribution; it
does not establish that a judgment is correct.

## Evaluate

The client APIs differ only in whether evaluation is awaited. With the default reqwest
feature:

```rust
use typesafe_ai::{Request, ReqwestClient, ReqwestError, Response};

async fn evaluate(
    client: &ReqwestClient,
    request: &Request,
) -> Result<Response, ReqwestError> {
    client.evaluate(request).await
}
```

With the ureq feature:

```rust
use typesafe_ai::{Request, Response, UreqClient, UreqError};

fn evaluate(client: &UreqClient, request: &Request) -> Result<Response, UreqError> {
    client.evaluate(request)
}
```

`AsyncClient` and `SyncClient` support code that is generic over a backend. Concrete
clients have inherent `evaluate` methods, so ordinary use does not require either trait.

The repository includes complete programs for both clients:

```console
cargo run --example async_evaluate
cargo run --no-default-features --features ureq,rustls-tls --example blocking_evaluate
```

Set `TYPESAFE_API_KEY` before running either example. For detailed service semantics, see
the [TypeSafe API reference](https://docs.typesafe.ai/api) and
[primitive guide](https://docs.typesafe.ai/primitives).

## List available models

Both concrete clients can list the models and aliases available to the authenticated account.
The response includes the model description, release date, request metadata, and original body:

```rust
use typesafe_ai::{ReqwestClient, ReqwestError};

async fn list_models(client: &ReqwestClient) -> Result<(), ReqwestError> {
    let response = client.list_models().await?;
    for model in response.models {
        println!("{} ({})", model.name, model.release_date);
    }
    Ok(())
}
```

Use `UreqClient::list_models` the same way without awaiting. Pin a versioned model ID when
you need stable behavior; aliases such as `jev-latest` can move to a newer release.

## A larger async example

[`tsg`](../examples/tsg/README.md) combines the async client with Tokio and clap to
search local text collections by meaning, including Markdown, prose, and code:

```console
cargo run --example tsg -- find 'Laws relevant to operating a food truck' laws/
cargo run --example tsg -- grep 'Does this passage impose a permit requirement?' laws/
```

`find` ranks passages useful for the supplied topic, task, or question, including
relevant definitions and exceptions. `grep` independently judges whether each
passage satisfies the supplied condition, allowing any number of matches.
Both return original source with file and line locations. The example demonstrates
bounded concurrent requests, cancellation, and explicit scan coverage; model judgments
can still miss relevant passages.
