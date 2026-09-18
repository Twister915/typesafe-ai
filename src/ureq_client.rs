use std::fmt;
use std::io::Read;
use std::thread;
use std::time::Instant;

use http::header::AUTHORIZATION;

use crate::transport::{RawResponse, ValidatedConfig, is_retryable, retry_delay};
use crate::{
    ClientConfig, Error, EvaluationEvent, EvaluationFailure, ModelsResponse, Request, Response,
    SyncClient,
};

/// Errors returned by [`UreqClient`].
pub type UreqError = Error<ureq::Error>;

/// Blocking TypeSafe client backed by ureq.
///
/// ```no_run
/// use typesafe_ai::{Question, Request, UreqClient};
///
/// # fn evaluate() -> Result<(), Box<dyn std::error::Error>> {
/// let client = UreqClient::new(std::env::var("TYPESAFE_API_KEY")?)?;
/// let request = Request::new("Please refund duplicate charge 1042.")
///     .with_question("is_billing", Question::noul("Is this a billing request?"));
/// let response = client.evaluate(&request)?;
/// # let _ = response;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct UreqClient {
    agent: ureq::Agent,
    config: ValidatedConfig,
}

impl UreqClient {
    /// Creates a client with [`ClientConfig::default`].
    ///
    /// The API key is marked sensitive and omitted from the client's debug output.
    pub fn new(api_key: impl Into<String>) -> Result<Self, UreqError> {
        Self::with_config(api_key, ClientConfig::default())
    }

    /// Creates a client with explicit shared configuration.
    pub fn with_config(
        api_key: impl Into<String>,
        config: ClientConfig,
    ) -> Result<Self, UreqError> {
        let config = ValidatedConfig::new(api_key.into(), config)
            .map_err(Error::with_transport::<ureq::Error>)?;
        let agent_config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(config.timeout))
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(agent_config),
            config,
        })
    }

    /// Creates a client around an existing ureq agent.
    ///
    /// Custom transport and resolver behavior from the agent is preserved. TypeSafe's
    /// status, redirect, and timeout policies are applied to each request.
    pub fn with_agent(
        api_key: impl Into<String>,
        config: ClientConfig,
        agent: ureq::Agent,
    ) -> Result<Self, UreqError> {
        Ok(Self {
            agent,
            config: ValidatedConfig::new(api_key.into(), config)
                .map_err(Error::with_transport::<ureq::Error>)?,
        })
    }

    /// Evaluates every question in `request` against its shared state.
    ///
    /// The request is validated before any HTTP exchange. HTTP 429 and 529 responses are
    /// retried according to [`ClientConfig::max_retries`].
    pub fn evaluate(&self, request: &Request) -> Result<Response, UreqError> {
        SyncClient::evaluate(self, request)
    }

    /// Lists the models and aliases available to the authenticated account.
    ///
    /// HTTP 429 and 529 responses are retried according to [`ClientConfig::max_retries`].
    pub fn list_models(&self) -> Result<ModelsResponse, UreqError> {
        let mut retries = 0_u32;
        loop {
            let attempt = u64::from(retries) + 1;
            let raw = self.send_models_attempt()?;
            if raw.status.is_success() {
                return raw.into_models::<ureq::Error>();
            }

            let should_retry = is_retryable(raw.status) && retries < self.config.max_retries;
            let delay = should_retry
                .then(|| retry_delay(&raw.headers, retries))
                .flatten();
            let error = raw.into_api_error::<ureq::Error>(attempt);
            let Some(delay) = delay else {
                return Err(error);
            };
            retries += 1;
            thread::sleep(delay);
        }
    }

    /// Creates a lazy iterator over attempt failures and the terminal result.
    ///
    /// Construction performs no validation, waiting, or HTTP work. Each call to `next`
    /// drives at most one HTTP attempt and yields failures before any retry delay. See
    /// [`SyncClient::evaluate_events`] for retry timing and interruption behavior.
    ///
    /// ```no_run
    /// use typesafe_ai::{UreqClient, Request};
    ///
    /// # fn observe(client: &UreqClient, request: &Request) {
    /// for event in client.evaluate_events(request) {
    ///     println!("{event:?}");
    /// }
    /// # }
    /// ```
    pub fn evaluate_events(
        &self,
        request: &Request,
    ) -> impl Iterator<Item = EvaluationEvent<ureq::Error>> {
        let mut validated = false;
        let mut done = false;
        let mut retries = 0_u32;
        let mut retry_deadline: Option<Instant> = None;

        std::iter::from_fn(move || {
            if done {
                return None;
            }
            if !validated {
                validated = true;
                if let Err(error) = request.validate() {
                    done = true;
                    return Some(EvaluationEvent::AttemptFailed(EvaluationFailure {
                        error: error.with_transport::<ureq::Error>(),
                        attempt: 0,
                        retry_delay: None,
                    }));
                }
            }
            if let Some(deadline) = retry_deadline.take() {
                thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }

            let attempt = u64::from(retries) + 1;
            let raw = match self.send_attempt(request) {
                Ok(raw) => raw,
                Err(error) => {
                    done = true;
                    return Some(EvaluationEvent::AttemptFailed(EvaluationFailure {
                        error,
                        attempt,
                        retry_delay: None,
                    }));
                }
            };
            if raw.status.is_success() {
                done = true;
                return Some(match raw.into_response::<ureq::Error>() {
                    Ok(response) => EvaluationEvent::Success(response),
                    Err(error) => EvaluationEvent::AttemptFailed(EvaluationFailure {
                        error,
                        attempt,
                        retry_delay: None,
                    }),
                });
            }

            let should_retry = is_retryable(raw.status) && retries < self.config.max_retries;
            let delay = should_retry
                .then(|| retry_delay(&raw.headers, retries))
                .flatten();
            if let Some(delay) = delay {
                retries += 1;
                retry_deadline = Some(Instant::now() + delay);
            } else {
                done = true;
            }
            Some(EvaluationEvent::AttemptFailed(EvaluationFailure {
                error: raw.into_api_error::<ureq::Error>(attempt),
                attempt,
                retry_delay: delay,
            }))
        })
    }

    fn send_attempt(&self, request: &Request) -> Result<RawResponse, UreqError> {
        let mut response = self
            .agent
            .post(self.config.endpoint.as_str())
            .config()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(self.config.timeout))
            .build()
            .header(AUTHORIZATION, self.config.authorization.clone())
            .send_json(request)
            .map_err(|error| map_transport_error(error, self.config.timeout))?;
        let status = response.status();
        let headers = response.headers().clone();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|error| map_transport_error(ureq::Error::from(error), self.config.timeout))?;
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }

    fn send_models_attempt(&self) -> Result<RawResponse, UreqError> {
        let mut response = self
            .agent
            .get(self.config.models_endpoint.as_str())
            .config()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(self.config.timeout))
            .build()
            .header(AUTHORIZATION, self.config.authorization.clone())
            .call()
            .map_err(|error| map_transport_error(error, self.config.timeout))?;
        let status = response.status();
        let headers = response.headers().clone();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|error| map_transport_error(ureq::Error::from(error), self.config.timeout))?;
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }
}

fn map_transport_error(error: ureq::Error, timeout: std::time::Duration) -> UreqError {
    match error {
        ureq::Error::Timeout(_) => Error::Timeout { timeout },
        error => Error::Transport(error),
    }
}

impl SyncClient for UreqClient {
    type TransportError = ureq::Error;

    fn evaluate_events(
        &self,
        request: &Request,
    ) -> impl Iterator<Item = EvaluationEvent<Self::TransportError>> {
        UreqClient::evaluate_events(self, request)
    }
}

impl fmt::Debug for UreqClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UreqClient")
            .field("endpoint", &self.config.endpoint)
            .field("authorization", &"<redacted>")
            .field("max_retries", &self.config.max_retries)
            .field("timeout", &self.config.timeout)
            .finish_non_exhaustive()
    }
}
