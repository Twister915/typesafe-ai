# Configuration and errors

Both backends use the same `ClientConfig`. By default they call
`https://api.typesafe.ai/v1/systemone`, allow 60 seconds for each complete attempt, and
make at most two retries after the initial attempt.

```rust
use std::time::Duration;

use typesafe_ai::{ClientConfig, ReqwestClient};

fn build() -> Result<ReqwestClient, typesafe_ai::ReqwestError> {
    let config = ClientConfig {
        timeout: Duration::from_secs(20),
        max_retries: 0,
        ..ClientConfig::default()
    };
    ReqwestClient::with_config("test-api-key", config)
}
```

Path prefixes in `base_url` are preserved when `/v1/systemone` is appended. API keys are
marked sensitive and omitted from client debug output.

## Retry behavior

The clients retry only HTTP 429 and 529 responses. Backoff starts at 250 milliseconds,
doubles to an 8-second cap, and honors longer valid server retry delays up to 60 seconds.
If the server asks for a delay longer than 60 seconds, the client returns the last API
error. Transport failures, timeouts, decoding failures, and other HTTP statuses are not
retried.

Timeouts apply to each complete attempt, including its response body. Retry progress is
available through the [event API](progress-events.md).

## Custom transports

`ReqwestClient::with_http_client` keeps the injected reqwest client's timeout, redirect,
and retry policies. Its timeout can end an attempt earlier, while redirects or internal
retries can change the documented request limits.

`UreqClient::with_agent` preserves the injected agent's transport and resolver behavior.
The TypeSafe client applies its status, redirect, and timeout policy to each request.

## Errors and response fidelity

Backend methods return `ReqwestError` or `UreqError`, aliases of the generic `Error` type.
Matching `Error::Transport(source)` gives direct access to the concrete reqwest or ureq
error. API and decoding failures retain the status, request ID, headers, and original body;
inspect them through `Error::status`, `Error::request_id`, `Error::headers`, and
`Error::body`. Display text omits credentials and API response bodies.

For non-success API responses, `ApiErrorDetails::try_from(error)` consumes the error and returns a
best-effort structured view of response shapes recognized by this crate. It returns the original
error when the body is empty, malformed, or not a recognized shape (and for errors that did not
receive an API response), so status, headers, request ID, and raw bytes remain available.
Malformed entries in a validation array are skipped; the original error is returned when no
message or valid entries remain.
The raw response bytes are never discarded: use `Error::body()` to parse an application-specific
or newer format yourself. A successful response that cannot be decoded as the SDK response is a
`Decode` error; its body is also available through `Error::body()`, but it is not parsed as API
error details. When retries are enabled, the terminal error contains the final attempt's body;
inspect `EvaluationFailure::error.body()` while consuming the event API to examine earlier
attempts.

Numeric answers, probabilities, and confidence values use `f64`, so they may not preserve
the decimal spelling in JSON. Successful responses retain the original bytes in
`Response::raw_body`. Applications that need exact decimal handling can parse those bytes
directly with an arbitrary-precision parser rather than converting the `f64` fields.

Raw bodies and headers can contain application data. Log them only when that is suitable
for the application's privacy and retention rules.
