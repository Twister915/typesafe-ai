use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;

const DEFAULT_MODEL: &str = "jev-latest";

/// State and typed questions sent to the System One evaluation endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// State for the model to evaluate.
    ///
    /// The top-level value must be a string, object, or array.
    pub state: Value,
    /// Model identifier. Defaults to `jev-latest`.
    pub model: String,
    /// Questions keyed by caller-selected identifiers.
    ///
    /// An identifier correlates its question with the returned answer; it is not used for
    /// inference.
    pub questions: BTreeMap<String, Question>,
}

impl Request {
    /// Creates a request using `jev-latest` and no questions.
    pub fn new(state: impl Into<Value>) -> Self {
        Self {
            state: state.into(),
            model: DEFAULT_MODEL.to_owned(),
            questions: BTreeMap::new(),
        }
    }

    /// Sets the model identifier.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Adds a question and returns the request.
    ///
    /// A later question with the same ID replaces the earlier one.
    #[must_use]
    pub fn with_question(mut self, id: impl Into<String>, question: Question) -> Self {
        self.questions.insert(id.into(), question);
        self
    }

    /// Inserts a question, returning a previous question with the same id.
    pub fn insert_question(
        &mut self,
        id: impl Into<String>,
        question: Question,
    ) -> Option<Question> {
        self.questions.insert(id.into(), question)
    }

    /// Validates the request against documented API shape constraints.
    ///
    /// Clients call this before sending a request. It is also available for applications
    /// that want to validate earlier.
    pub fn validate(&self) -> Result<(), Error> {
        if !is_state(&self.state) {
            return Err(Error::Validation {
                field: "state".to_owned(),
                message: "must be a string, object, or array".to_owned(),
            });
        }

        if self.questions.is_empty() {
            return Err(Error::Validation {
                field: "questions".to_owned(),
                message: "must contain at least one question".to_owned(),
            });
        }

        for (id, question) in &self.questions {
            question.validate(id)?;
        }
        Ok(())
    }
}

impl Default for Request {
    fn default() -> Self {
        Self::new(Value::String(String::new()))
    }
}

/// Criteria defining the positive and negative sides of a Noul question.
///
/// Each side accepts a string, object, array, or JSON null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulCriteria {
    /// Description of what a positive answer means, or null when unspecified.
    #[serde(default, rename = "true")]
    pub when_true: Value,
    /// Description of what a negative answer means, or null when unspecified.
    #[serde(default, rename = "false")]
    pub when_false: Value,
}

impl NoulCriteria {
    /// Creates positive and negative Noul criteria.
    pub fn new(when_true: impl Into<Value>, when_false: impl Into<Value>) -> Self {
        Self {
            when_true: when_true.into(),
            when_false: when_false.into(),
        }
    }
}

/// A typed judgment for System One to make against request state.
///
/// Instructions and criteria descriptions accept strings, objects, arrays, or JSON null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// A yes/no judgment returned as the probability of yes.
    Noul {
        /// The judgment the model should make. Missing wire values deserialize as JSON null.
        #[serde(default)]
        instructions: Value,
        /// Optional descriptions of the positive and negative answers.
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Selection of one label from a caller-defined set.
    Choice {
        /// The decision the model should make. Missing wire values deserialize as JSON null.
        #[serde(default)]
        instructions: Value,
        /// Options keyed by their labels.
        criteria: BTreeMap<String, Value>,
    },
    /// A rating along an ordered caller-defined rubric.
    Score {
        /// The rating the model should make. Missing wire values deserialize as JSON null.
        #[serde(default)]
        instructions: Value,
        /// Ordered descriptions of the score levels.
        criteria: Vec<Value>,
    },
}

impl Question {
    /// Creates a Noul question without explicit yes and no criteria.
    pub fn noul(instructions: impl Into<Value>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    /// Creates a Noul question with explicit positive and negative criteria.
    pub fn noul_with_criteria(instructions: impl Into<Value>, criteria: NoulCriteria) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some(criteria),
        }
    }

    /// Creates a Choice question from options keyed by their returned labels.
    ///
    /// At least one option is required when the request is validated.
    pub fn choice(instructions: impl Into<Value>, criteria: BTreeMap<String, Value>) -> Self {
        Self::Choice {
            instructions: instructions.into(),
            criteria,
        }
    }

    /// Creates a Score question from ordered level descriptions.
    ///
    /// At least two levels are required when the request is validated.
    pub fn score(instructions: impl Into<Value>, criteria: Vec<Value>) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria,
        }
    }

    fn validate(&self, id: &str) -> Result<(), Error> {
        let instructions = match self {
            Self::Noul { instructions, .. }
            | Self::Choice { instructions, .. }
            | Self::Score { instructions, .. } => instructions,
        };
        validate_entry(instructions, format!("questions.{id}.instructions"))?;

        match self {
            Self::Noul {
                criteria: Some(criteria),
                ..
            } => {
                validate_entry(&criteria.when_true, format!("questions.{id}.criteria.true"))?;
                validate_entry(
                    &criteria.when_false,
                    format!("questions.{id}.criteria.false"),
                )
            }
            Self::Noul { criteria: None, .. } => Ok(()),
            Self::Choice { criteria, .. } => {
                if criteria.is_empty() {
                    return Err(Error::Validation {
                        field: format!("questions.{id}.criteria"),
                        message: "must contain at least one option".to_owned(),
                    });
                }
                for (option, description) in criteria {
                    validate_entry(description, format!("questions.{id}.criteria.{option}"))?;
                }
                Ok(())
            }
            Self::Score { criteria, .. } => {
                if criteria.len() < 2 {
                    return Err(Error::Validation {
                        field: format!("questions.{id}.criteria"),
                        message: "must contain at least two levels".to_owned(),
                    });
                }
                for (level, description) in criteria.iter().enumerate() {
                    validate_entry(description, format!("questions.{id}.criteria[{level}]"))?;
                }
                Ok(())
            }
        }
    }
}

fn is_state(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Object(_) | Value::Array(_))
}

fn validate_entry(value: &Value, field: String) -> Result<(), Error> {
    if matches!(
        value,
        Value::String(_) | Value::Object(_) | Value::Array(_) | Value::Null
    ) {
        Ok(())
    } else {
        Err(Error::Validation {
            field,
            message: "must be a string, object, array, or null".to_owned(),
        })
    }
}
