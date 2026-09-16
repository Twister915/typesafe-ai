use std::collections::BTreeMap;

use http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A successful System One response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Response {
    /// Model that produced the answers.
    pub model: String,
    /// Answers keyed by the request's question identifiers.
    pub answers: BTreeMap<String, Answer>,
    /// Token counts, when reported by the API.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Value of the `x-typesafe-request-id` response header, when present.
    #[serde(skip)]
    pub request_id: Option<String>,
    /// HTTP response headers.
    #[serde(skip)]
    pub headers: HeaderMap,
    /// Original response body bytes, preserving JSON number spelling and precision.
    ///
    /// Populated by the HTTP clients; empty when deserializing this type directly.
    /// Parse these bytes with an exact-decimal parser when binary floating-point
    /// approximations in [`Answer`] are unsuitable.
    #[serde(skip)]
    pub raw_body: Vec<u8>,
}

impl Response {
    /// Returns an answer by its question identifier.
    pub fn answer(&self, id: &str) -> Option<&Answer> {
        self.answers.get(id)
    }
}

/// Token counts reported for a request.
///
/// Counts remain optional for forward compatibility with responses that omit either
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input tokens, when reported.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Number of output tokens, when reported.
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

/// A typed answer returned by System One.
///
/// Numeric fields use `f64` for convenient comparisons and arithmetic. For exact JSON
/// number preservation, use [`Response::raw_body`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// A yes/no answer represented as the probability of yes.
    Noul {
        /// Probability of a positive answer.
        noul: f64,
    },
    /// The selected option and its probability distribution.
    Choice {
        /// Selected option.
        choice: String,
        /// Probability for each option.
        probabilities: BTreeMap<String, f64>,
        /// Confidence derived from the option distribution.
        confidence: f64,
    },
    /// An expected score and its probability distribution.
    Score {
        /// Probability-weighted position across the ordered score levels.
        score: f64,
        /// Rubric descriptions keyed by score level.
        legend: BTreeMap<String, Value>,
        /// Probability for each score level.
        probabilities: BTreeMap<String, f64>,
        /// Confidence derived from the level distribution.
        confidence: f64,
    },
}

impl Answer {
    /// Returns the Noul probability for a Noul answer.
    pub fn noul(&self) -> Option<f64> {
        match self {
            Self::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    /// Returns the selected label for a Choice answer.
    pub fn choice(&self) -> Option<&str> {
        match self {
            Self::Choice { choice, .. } => Some(choice),
            _ => None,
        }
    }

    /// Returns the expected score for a Score answer.
    pub fn score(&self) -> Option<f64> {
        match self {
            Self::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// Returns distribution-derived confidence for Choice and Score answers.
    ///
    /// This is `None` for Noul because its probability is the answer itself.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Choice { confidence, .. } | Self::Score { confidence, .. } => Some(*confidence),
            Self::Noul { .. } => None,
        }
    }

    /// Returns the complete distribution for Choice and Score answers.
    pub fn probabilities(&self) -> Option<&BTreeMap<String, f64>> {
        match self {
            Self::Choice { probabilities, .. } | Self::Score { probabilities, .. } => {
                Some(probabilities)
            }
            Self::Noul { .. } => None,
        }
    }
}
