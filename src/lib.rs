#![warn(missing_docs)]
#![forbid(unsafe_code)]

//! Blocking and asynchronous clients for TypeSafe's System One API.
//!
//! Requests contain shared [`Request::state`] and one or more typed [`Question`]s. The
//! service evaluates those questions independently and returns an [`Answer`] under each
//! caller-selected question ID.
//!
//! The default `reqwest` feature provides the asynchronous `ReqwestClient`. Enable the
//! `ureq` feature for the blocking `UreqClient`. The `rustls-tls` feature enables rustls
//! on each selected backend. With no backend feature, the data types and client traits
//! remain available without an HTTP implementation.
//!
//! # Request example
//!
//! The same request can be passed to either backend:
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use serde_json::json;
//! use typesafe_ai::{Question, Request};
//!
//! let request = Request::new("Help! My payouts have been failing for three days.")
//!     .with_question(
//!         "is_urgent",
//!         Question::noul("Does the customer convey urgency?"),
//!     )
//!     .with_question(
//!         "department",
//!         Question::choice(
//!             "Which team should handle this?",
//!             BTreeMap::from([
//!                 ("billing".into(), json!("Payments and invoices")),
//!                 ("technical".into(), json!("Bugs and outages")),
//!             ]),
//!         ),
//!     )
//!     .with_question(
//!         "frustration",
//!         Question::score(
//!             "How frustrated is the customer?",
//!             vec![json!("Calm"), json!("Frustrated"), json!("Very angry")],
//!         ),
//!     );
//!
//! assert_eq!(request.model, "jev-latest");
//! assert_eq!(request.questions.len(), 3);
//! ```

mod config;
mod error;
mod events;
mod request;
#[cfg(feature = "reqwest")]
mod reqwest_client;
mod response;
#[cfg(any(feature = "reqwest", feature = "ureq"))]
mod transport;
#[cfg(feature = "ureq")]
mod ureq_client;

pub use config::ClientConfig;
pub use error::Error;
pub use events::{EvaluationEvent, EvaluationFailure};
pub use futures_core::Stream;
pub use request::{NoulCriteria, Question, Request};
#[cfg(feature = "reqwest")]
pub use reqwest_client::{ReqwestClient, ReqwestError};
pub use response::{Answer, Response, Usage};
#[cfg(feature = "ureq")]
pub use ureq_client::{UreqClient, UreqError};

use std::future::Future;

/// A backend-neutral asynchronous TypeSafe client.
///
/// Use this trait when code must be generic over asynchronous backends. Concrete clients
/// also provide an inherent `evaluate` method.
pub trait AsyncClient {
    /// The concrete transport error produced by this implementation.
    type TransportError: std::error::Error + Send + Sync + 'static;

    /// Creates a lazy stream of attempt failures and the final evaluation result.
    ///
    /// Polling drives validation, HTTP work, and retry delays. Yield retryable failures
    /// before waiting, and end after success or a failure with no scheduled retry.
    /// Dropping the stream cancels local pending work, not necessarily server processing.
    fn evaluate_events<'a>(
        &'a self,
        request: &'a Request,
    ) -> impl Stream<Item = EvaluationEvent<Self::TransportError>> + Send + 'a;

    /// Drives the event stream to its terminal result.
    fn evaluate<'a>(
        &'a self,
        request: &'a Request,
    ) -> impl Future<Output = Result<Response, Error<Self::TransportError>>> + Send + 'a {
        let events = self.evaluate_events(request);
        async move {
            let mut events = std::pin::pin!(events);
            while let Some(event) =
                std::future::poll_fn(|context| events.as_mut().poll_next(context)).await
            {
                if let Some(result) = event.into_result() {
                    return result;
                }
            }
            Err(Error::IncompleteEvaluation)
        }
    }
}

/// A backend-neutral blocking TypeSafe client.
///
/// Use this trait when code must be generic over blocking backends. Concrete clients also
/// provide an inherent `evaluate` method.
pub trait SyncClient {
    /// The concrete transport error produced by this implementation.
    type TransportError: std::error::Error + Send + Sync + 'static;

    /// Creates a lazy iterator of attempt failures and the final evaluation result.
    ///
    /// Each `next()` drives validation, retry waiting, and at most one HTTP attempt.
    /// Dropping between calls stops further work; it cannot interrupt a running `next()`.
    fn evaluate_events(
        &self,
        request: &Request,
    ) -> impl Iterator<Item = EvaluationEvent<Self::TransportError>>;

    /// Drives the event iterator to its terminal result.
    fn evaluate(&self, request: &Request) -> Result<Response, Error<Self::TransportError>> {
        for event in self.evaluate_events(request) {
            if let Some(result) = event.into_result() {
                return result;
            }
        }
        Err(Error::IncompleteEvaluation)
    }
}
