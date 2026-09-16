use std::convert::Infallible;
use std::time::Duration;

use http::{HeaderMap, StatusCode};
use thiserror::Error;

/// Errors returned while configuring or using a client.
#[derive(Debug, Error)]
pub enum Error<E = Infallible> {
    /// The request violates a documented API constraint.
    #[error("request validation failed at {field}: {message}")]
    Validation {
        /// Field containing the invalid value.
        field: String,
        /// Description of the constraint.
        message: String,
    },
    /// The client configuration is invalid.
    #[error("invalid client configuration: {0}")]
    Configuration(String),
    /// The HTTP exchange failed before a response was available.
    #[error("HTTP transport failed: {0}")]
    Transport(#[source] E),
    /// The configured per-attempt timeout elapsed.
    #[error("HTTP attempt timed out (configured attempt limit: {timeout:?})")]
    Timeout {
        /// Timeout configured for each attempt.
        timeout: Duration,
    },
    /// A successful HTTP response could not be decoded.
    #[error("failed to decode successful HTTP {status} response: {source}")]
    Decode {
        /// HTTP status returned by the API.
        status: StatusCode,
        /// Value of the request id response header, when present.
        request_id: Option<String>,
        /// HTTP response headers.
        headers: Box<HeaderMap>,
        /// Raw response bytes.
        body: Vec<u8>,
        /// JSON decoding error.
        #[source]
        source: serde_json::Error,
    },
    /// The API returned a non-success status.
    #[error("TypeSafe API returned HTTP {status} after {attempts} attempt(s)")]
    Api {
        /// HTTP status returned by the API.
        status: StatusCode,
        /// Value of the request id response header, when present.
        request_id: Option<String>,
        /// HTTP response headers.
        headers: Box<HeaderMap>,
        /// Raw response bytes, including non-JSON bodies.
        body: Vec<u8>,
        /// Requested server retry delay, when supplied and valid.
        retry_after: Option<Duration>,
        /// Total attempts made, including the initial request.
        attempts: u64,
    },
    /// An application-supplied event sequence ended before a terminal event.
    #[error("evaluation event sequence ended without a terminal result")]
    IncompleteEvaluation,
}

impl<E> Error<E> {
    /// Returns the associated HTTP status, when a response was received.
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Decode { status, .. } | Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Returns the TypeSafe request id, when one was received.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Decode { request_id, .. } | Self::Api { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }

    /// Returns raw response bytes for API and decode failures.
    pub fn body(&self) -> Option<&[u8]> {
        match self {
            Self::Decode { body, .. } | Self::Api { body, .. } => Some(body),
            _ => None,
        }
    }

    /// Returns response headers for API and decode failures.
    pub fn headers(&self) -> Option<&HeaderMap> {
        match self {
            Self::Decode { headers, .. } | Self::Api { headers, .. } => Some(headers.as_ref()),
            _ => None,
        }
    }
}

impl Error<Infallible> {
    /// Converts a backend-neutral error to one carrying a concrete transport error type.
    ///
    /// This is useful in custom client implementations after calling [`Request::validate`](crate::Request::validate).
    pub fn with_transport<E>(self) -> Error<E>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        match self {
            Self::Validation { field, message } => Error::Validation { field, message },
            Self::Configuration(message) => Error::Configuration(message),
            Self::Transport(never) => match never {},
            Self::Timeout { timeout } => Error::Timeout { timeout },
            Self::Decode {
                status,
                request_id,
                headers,
                body,
                source,
            } => Error::Decode {
                status,
                request_id,
                headers,
                body,
                source,
            },
            Self::Api {
                status,
                request_id,
                headers,
                body,
                retry_after,
                attempts,
            } => Error::Api {
                status,
                request_id,
                headers,
                body,
                retry_after,
                attempts,
            },
            Self::IncompleteEvaluation => Error::IncompleteEvaluation,
        }
    }
}
