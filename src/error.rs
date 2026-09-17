use std::convert::Infallible;
use std::time::Duration;

use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

/// Best-effort structured details extracted from a TypeSafe API error body.
///
/// This is intentionally a small view over the currently recognized response shapes rather
/// than a lossless representation of every possible API error. When parsing is not graceful,
/// [`Error::api_error_details`](crate::Error::api_error_details) returns `None`; callers can
/// always inspect the original bytes with [`Error::body`](crate::Error::body).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ApiErrorDetails {
    /// A human-readable message, when one was present in the response body.
    pub message: Option<String>,
    /// Validation issues from a FastAPI-style `detail` array.
    pub validation: Vec<ApiValidationError>,
}

/// One validation issue from a FastAPI-style TypeSafe API error response.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApiValidationError {
    /// Location of the invalid value. Segments can be field names or array indexes.
    #[serde(rename = "loc")]
    pub location: Vec<Value>,
    /// Human-readable validation message.
    #[serde(rename = "msg")]
    pub message: String,
    /// Machine-readable validation category.
    #[serde(rename = "type")]
    pub kind: String,
}

fn parse_api_error_details(body: &[u8]) -> Option<ApiErrorDetails> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    match value {
        Value::String(message) if !message.is_empty() => Some(ApiErrorDetails {
            message: Some(message.clone()),
            validation: Vec::new(),
        }),
        Value::Object(object) => parse_api_error_object(&object),
        _ => None,
    }
}

fn parse_api_error_object(object: &Map<String, Value>) -> Option<ApiErrorDetails> {
    let message = ["error", "message", "detail"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(message_value))
        .map(str::to_owned);
    let validation = object
        .get("detail")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    serde_json::from_value::<ApiValidationError>(entry.clone()).ok()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if message.is_none() && validation.is_empty() {
        None
    } else {
        Some(ApiErrorDetails {
            message,
            validation,
        })
    }
}

fn message_value(value: &Value) -> Option<&str> {
    if let Some(message) = value.as_str().filter(|message| !message.is_empty()) {
        return Some(message);
    }
    value.as_object().and_then(|object| {
        ["message", "msg", "detail"]
            .into_iter()
            .find_map(|key| object.get(key).and_then(message_value))
    })
}

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

    /// Returns best-effort structured details from a non-success API response.
    ///
    /// This returns `None` for local errors, successful-response decoding failures, empty or
    /// malformed bodies, and response shapes that this version does not recognize. The
    /// original response bytes remain available through [`Self::body`] for custom parsing.
    pub fn api_error_details(&self) -> Option<ApiErrorDetails> {
        let Self::Api { body, .. } = self else {
            return None;
        };
        parse_api_error_details(body)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_validation_details() {
        let details = parse_api_error_details(
            br#"{
                "detail": [
                    {
                        "loc": ["body", "questions", "urgent", "criteria", 1],
                        "msg": "Input should be a valid string",
                        "type": "string_type"
                    }
                ]
            }"#,
        )
        .expect("validation details");

        assert_eq!(details.message, None);
        assert_eq!(details.validation.len(), 1);
        assert_eq!(details.validation[0].location[4], Value::from(1));
        assert_eq!(
            details.validation[0].message,
            "Input should be a valid string"
        );
        assert_eq!(details.validation[0].kind, "string_type");
    }

    #[test]
    fn parses_common_message_shapes() {
        for (body, expected) in [
            (
                br#"{"message":"top-level message"}"#.as_slice(),
                "top-level message",
            ),
            (br#"{"error":"error message"}"#.as_slice(), "error message"),
            (
                br#"{"error":{"message":"nested message"}}"#.as_slice(),
                "nested message",
            ),
            (
                br#"{"detail":"detail message"}"#.as_slice(),
                "detail message",
            ),
            (
                br#"{"detail":{"msg":"nested detail"}}"#.as_slice(),
                "nested detail",
            ),
            (br#""nested string body""#.as_slice(), "nested string body"),
        ] {
            assert_eq!(
                parse_api_error_details(body).and_then(|details| details.message),
                Some(expected.to_owned())
            );
        }
    }

    #[test]
    fn prefers_specific_error_messages_and_keeps_validations() {
        let details = parse_api_error_details(
            br#"{
                "error": "specific failure",
                "message": "generic failure",
                "detail": [
                    {"loc": ["body", "model"], "msg": "required", "type": "missing"},
                    {"msg": "future validation shape"}
                ]
            }"#,
        )
        .expect("usable error details");

        assert_eq!(details.message.as_deref(), Some("specific failure"));
        assert_eq!(details.validation.len(), 1);
        assert_eq!(details.validation[0].message, "required");
    }

    #[test]
    fn returns_none_for_unknown_or_malformed_shapes() {
        for body in [
            b"".as_slice(),
            b"not json".as_slice(),
            b"\"\"".as_slice(),
            br#"{"detail":[]}"#.as_slice(),
            br#"{"detail":[{"msg":"missing location and type"}]}"#.as_slice(),
            br#"{"unexpected":{"value":true}}"#.as_slice(),
            br#"null"#.as_slice(),
        ] {
            assert_eq!(
                parse_api_error_details(body),
                None,
                "expected body not to parse"
            );
        }
    }
}
