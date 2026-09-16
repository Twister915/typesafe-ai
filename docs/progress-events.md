# Progress events

`evaluate` is the simplest API: it handles retry delays and returns a successful response
or terminal error. Use `evaluate_events` when an application needs to display progress,
record individual failures, or observe retry scheduling.

Each `EvaluationEvent::Failed` contains:

- `error`: the validation, transport, timeout, decoding, or API error;
- `attempt`: zero for local validation, then one-based HTTP attempt numbers;
- `retry_delay`: the planned delay, or `None` when the failure is terminal.

A successful evaluation ends with `EvaluationEvent::Success(Response)`. The library owns
the retry policy and delay in both APIs.

## Asynchronous stream

The reqwest client returns a lazy `Stream`. Constructing it performs no validation,
waiting, or HTTP work. The crate reexports the `Stream` trait, so no extension crate is
needed:

```rust
use std::{future::poll_fn, pin::pin};

use typesafe_ai::{EvaluationEvent, Request, ReqwestClient, Stream};

async fn observe(client: &ReqwestClient, request: &Request) {
    let events = client.evaluate_events(request);
    let mut events = pin!(events);

    while let Some(event) = poll_fn(|cx| events.as_mut().poll_next(cx)).await {
        match event {
            EvaluationEvent::Failed(failure) => {
                eprintln!("attempt {} failed: {}", failure.attempt, failure.error);
                if let Some(delay) = failure.retry_delay {
                    eprintln!("retrying in {delay:?}");
                }
            }
            EvaluationEvent::Success(response) => {
                println!("received {} answers", response.answers.len());
            }
        }
    }
}
```

Dropping the stream cancels pending local work. It cannot guarantee cancellation of a
request the server has already received.

## Blocking iterator

The ureq client returns a lazy `Iterator`:

```rust
use typesafe_ai::{EvaluationEvent, Request, UreqClient};

fn observe(client: &UreqClient, request: &Request) {
    for event in client.evaluate_events(request) {
        match event {
            EvaluationEvent::Failed(failure) => {
                eprintln!("attempt {} failed: {}", failure.attempt, failure.error);
            }
            EvaluationEvent::Success(response) => {
                println!("received {} answers", response.answers.len());
            }
        }
    }
}
```

Each `next` call performs at most one HTTP attempt. Dropping the iterator between calls
prevents later attempts, but cannot interrupt an active `next` call.

Failures are yielded before the library waits. The retry deadline starts immediately
before the event is returned, so time spent processing it counts toward the backoff and
the next poll or `next` waits only for the remainder.

Complete programs are available in `examples/async_events.rs` and
`examples/blocking_events.rs`:

```console
cargo run --example async_events
cargo run --no-default-features --features ureq,rustls-tls --example blocking_events
```
