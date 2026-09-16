use std::fmt;

use futures_util::{StreamExt, stream};
use http::header::AUTHORIZATION;
use tokio::time::Instant;

use crate::transport::{RawResponse, ValidatedConfig, is_retryable, retry_delay};
use crate::{
    AsyncClient, ClientConfig, Error, EvaluationEvent, EvaluationFailure, Request, Response, Stream,
};

/// Errors returned by [`ReqwestClient`].
pub type ReqwestError = Error<reqwest::Error>;

/// Asynchronous TypeSafe client backed by reqwest.
///
/// ```no_run
/// use typesafe_ai::{Question, Request, ReqwestClient};
///
/// # async fn evaluate() -> Result<(), Box<dyn std::error::Error>> {
/// let client = ReqwestClient::new(std::env::var("TYPESAFE_API_KEY")?)?;
/// let request = Request::new("Please refund duplicate charge 1042.")
///     .with_question("is_billing", Question::noul("Is this a billing request?"));
/// let response = client.evaluate(&request).await?;
/// # let _ = response;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ReqwestClient {
    http: reqwest::Client,
    config: ValidatedConfig,
}

struct EvaluationState<'a> {
    client: &'a ReqwestClient,
    request: &'a Request,
    validated: bool,
    done: bool,
    retries: u32,
    retry_deadline: Option<Instant>,
}

impl ReqwestClient {
    /// Creates a client with [`ClientConfig::default`].
    ///
    /// The API key is marked sensitive and omitted from the client's debug output.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ReqwestError> {
        Self::with_config(api_key, ClientConfig::default())
    }

    /// Creates a client with explicit shared configuration.
    pub fn with_config(
        api_key: impl Into<String>,
        config: ClientConfig,
    ) -> Result<Self, ReqwestError> {
        let config = ValidatedConfig::new(api_key.into(), config)
            .map_err(Error::with_transport::<reqwest::Error>)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(config.timeout)
            .build()
            .map_err(Error::Transport)?;
        Ok(Self { http, config })
    }

    /// Creates a client around an existing reqwest client.
    ///
    /// The injected client's timeout, redirect, and retry policies remain in effect. They
    /// may end a request sooner or cause more requests than [`ClientConfig`] alone implies.
    /// The TypeSafe client still applies its own complete-attempt timeout and retry policy.
    pub fn with_http_client(
        api_key: impl Into<String>,
        config: ClientConfig,
        http: reqwest::Client,
    ) -> Result<Self, ReqwestError> {
        Ok(Self {
            http,
            config: ValidatedConfig::new(api_key.into(), config)
                .map_err(Error::with_transport::<reqwest::Error>)?,
        })
    }

    /// Evaluates every question in `request` against its shared state.
    ///
    /// The request is validated before any HTTP exchange. HTTP 429 and 529 responses are
    /// retried according to [`ClientConfig::max_retries`].
    pub async fn evaluate(&self, request: &Request) -> Result<Response, ReqwestError> {
        AsyncClient::evaluate(self, request).await
    }

    /// Creates a lazy stream over attempt failures and the terminal result.
    ///
    /// Construction performs no validation, waiting, or HTTP work. Polling drives the
    /// evaluation and yields each failure before any retry delay. See
    /// [`AsyncClient::evaluate_events`] for retry timing and cancellation behavior.
    ///
    /// ```no_run
    /// use std::{future::poll_fn, pin::pin};
    ///
    /// use typesafe_ai::{ReqwestClient, Request, Stream};
    ///
    /// # async fn observe(client: &ReqwestClient, request: &Request) {
    /// let events = client.evaluate_events(request);
    /// let mut events = pin!(events);
    /// while let Some(event) = poll_fn(|cx| events.as_mut().poll_next(cx)).await {
    ///     println!("{event:?}");
    /// }
    /// # }
    /// ```
    pub fn evaluate_events<'a>(
        &'a self,
        request: &'a Request,
    ) -> impl Stream<Item = EvaluationEvent<reqwest::Error>> + Send + 'a {
        let state = EvaluationState {
            client: self,
            request,
            validated: false,
            done: false,
            retries: 0,
            retry_deadline: None,
        };
        stream::unfold(state, |mut state| async move {
            if state.done {
                return None;
            }
            if !state.validated {
                state.validated = true;
                if let Err(error) = state.request.validate() {
                    state.done = true;
                    let event = EvaluationEvent::Failed(EvaluationFailure {
                        error: error.with_transport::<reqwest::Error>(),
                        attempt: 0,
                        retry_delay: None,
                    });
                    return Some((event, state));
                }
            }
            if let Some(deadline) = state.retry_deadline.take() {
                tokio::time::sleep_until(deadline).await;
            }

            let attempt = u64::from(state.retries) + 1;
            let raw = match state.client.send_attempt(state.request).await {
                Ok(raw) => raw,
                Err(error) => {
                    state.done = true;
                    let event = EvaluationEvent::Failed(EvaluationFailure {
                        error,
                        attempt,
                        retry_delay: None,
                    });
                    return Some((event, state));
                }
            };
            if raw.status.is_success() {
                state.done = true;
                let event = match raw.into_response::<reqwest::Error>() {
                    Ok(response) => EvaluationEvent::Success(response),
                    Err(error) => EvaluationEvent::Failed(EvaluationFailure {
                        error,
                        attempt,
                        retry_delay: None,
                    }),
                };
                return Some((event, state));
            }

            let should_retry =
                is_retryable(raw.status) && state.retries < state.client.config.max_retries;
            let delay = should_retry
                .then(|| retry_delay(&raw.headers, state.retries))
                .flatten();
            if let Some(delay) = delay {
                state.retries += 1;
                state.retry_deadline = Some(Instant::now() + delay);
            } else {
                state.done = true;
            }
            let event = EvaluationEvent::Failed(EvaluationFailure {
                error: raw.into_api_error::<reqwest::Error>(attempt),
                attempt,
                retry_delay: delay,
            });
            Some((event, state))
        })
        .fuse()
    }

    async fn send_attempt(&self, request: &Request) -> Result<RawResponse, ReqwestError> {
        let exchange = async {
            let response = self
                .http
                .post(self.config.endpoint.clone())
                .header(AUTHORIZATION, self.config.authorization.clone())
                .json(request)
                .send()
                .await?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes().await?.to_vec();
            Ok::<_, reqwest::Error>(RawResponse {
                status,
                headers,
                body,
            })
        };

        match tokio::time::timeout(self.config.timeout, exchange).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) if error.is_timeout() => Err(Error::Timeout {
                timeout: self.config.timeout,
            }),
            Ok(Err(error)) => Err(Error::Transport(error)),
            Err(_) => Err(Error::Timeout {
                timeout: self.config.timeout,
            }),
        }
    }
}

impl AsyncClient for ReqwestClient {
    type TransportError = reqwest::Error;

    fn evaluate_events<'a>(
        &'a self,
        request: &'a Request,
    ) -> impl Stream<Item = EvaluationEvent<Self::TransportError>> + Send + 'a {
        ReqwestClient::evaluate_events(self, request)
    }
}

impl fmt::Debug for ReqwestClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestClient")
            .field("endpoint", &self.config.endpoint)
            .field("authorization", &"<redacted>")
            .field("max_retries", &self.config.max_retries)
            .field("timeout", &self.config.timeout)
            .finish_non_exhaustive()
    }
}
