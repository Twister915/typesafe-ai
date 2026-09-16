use std::{convert::Infallible, time::Duration};

use crate::{Error, Response};

/// An observable outcome while evaluating a request.
///
/// A sequence can contain retryable failures followed by one success or terminal
/// failure. Consuming the iterator or stream drives the work; no background task runs.
#[derive(Debug)]
pub enum EvaluationEvent<E = Infallible> {
    /// An unsuccessful attempt, with information about the next retry.
    Failed(EvaluationFailure<E>),
    /// The completed evaluation. This is always the final event.
    Success(Response),
}

impl<E> EvaluationEvent<E> {
    /// Returns whether this event completes the evaluation.
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Failed(failure) => failure.retry_delay.is_none(),
            Self::Success(_) => true,
        }
    }

    pub(crate) fn into_result(self) -> Option<Result<Response, Error<E>>> {
        match self {
            Self::Failed(failure) if failure.retry_delay.is_none() => Some(Err(failure.error)),
            Self::Failed(_) => None,
            Self::Success(response) => Some(Ok(response)),
        }
    }
}

/// A failed attempt and its scheduled retry, if any.
#[derive(Debug)]
pub struct EvaluationFailure<E = Infallible> {
    /// The validation, transport, timeout, decoding, or HTTP error.
    pub error: Error<E>,
    /// One-based HTTP attempt number, or zero for local request validation failure.
    pub attempt: u64,
    /// Planned backoff before another attempt; `None` means terminal failure.
    ///
    /// The deadline starts before this event is yielded. Time spent handling the
    /// event counts toward the delay; the next poll or `next()` waits only for the
    /// remaining duration. Dropping the sequence prevents that retry.
    pub retry_delay: Option<Duration>,
}
