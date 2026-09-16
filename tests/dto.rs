use std::collections::BTreeMap;

use serde_json::{Value, json};
use test_case::test_case;
use typesafe_ai::{Answer, Error, NoulCriteria, Question, Request, Response};

#[test_case(json!("text"), true ; "string state")]
#[test_case(json!({"message": "text"}), true ; "object state")]
#[test_case(json!(["first", "second"]), true ; "array state")]
#[test_case(Value::Null, false ; "null state")]
#[test_case(json!(true), false ; "boolean state")]
#[test_case(json!(42), false ; "numeric state")]
fn validates_documented_state_shapes(state: Value, valid: bool) {
    let result = Request::new(state).validate();
    assert_eq!(result.is_ok(), valid);
}

#[test_case(Value::Null, true ; "null entry")]
#[test_case(json!("question"), true ; "string entry")]
#[test_case(json!({"question": "which"}), true ; "object entry")]
#[test_case(json!(["first", "second"]), true ; "array entry")]
#[test_case(json!(false), false ; "boolean entry")]
#[test_case(json!(3), false ; "numeric entry")]
fn validates_documented_entry_shapes(instructions: Value, valid: bool) {
    let request = Request::new("state").with_question("q", Question::noul(instructions));
    assert_eq!(request.validate().is_ok(), valid);
}

#[test_case(Question::choice("pick", BTreeMap::new()), "at least one option" ; "empty choice")]
#[test_case(Question::score("rate", vec![json!("only")]), "at least two levels" ; "one score level")]
fn validates_primitive_cardinality(question: Question, expected: &str) {
    let error = Request::new("state")
        .with_question("q", question)
        .validate()
        .expect_err("question should be invalid");
    assert!(error.to_string().contains(expected));
}

#[test]
fn serializes_structured_questions_with_the_wire_tags() {
    let mut choices = BTreeMap::new();
    choices.insert("billing".to_owned(), json!({"includes": ["refunds"]}));
    choices.insert("other".to_owned(), Value::Null);

    let request = Request::new(json!({"ticket": "refund please"}))
        .with_question(
            "urgent",
            Question::noul_with_criteria(
                json!({"question": "Is it urgent?"}),
                NoulCriteria::new(json!(["time-sensitive"]), Value::Null),
            ),
        )
        .with_question("team", Question::choice("Pick a team", choices))
        .with_question(
            "intensity",
            Question::score(
                "Rate intensity",
                vec![json!({"label": "low"}), json!(["high", "severe"])],
            ),
        );

    assert_eq!(
        serde_json::to_value(request).expect("serializes"),
        json!({
            "state": {"ticket": "refund please"},
            "model": "jev-latest",
            "questions": {
                "urgent": {
                    "type": "noul",
                    "instructions": {"question": "Is it urgent?"},
                    "criteria": {"true": ["time-sensitive"], "false": null}
                },
                "team": {
                    "type": "choice",
                    "instructions": "Pick a team",
                    "criteria": {
                        "billing": {"includes": ["refunds"]},
                        "other": null
                    }
                },
                "intensity": {
                    "type": "score",
                    "instructions": "Rate intensity",
                    "criteria": [{"label": "low"}, ["high", "severe"]]
                }
            }
        })
    );
}

#[test]
fn missing_noul_criterion_side_deserializes_as_null() {
    let question: Question = serde_json::from_value(json!({
        "type": "noul",
        "instructions": "Check it",
        "criteria": {"true": "matches"}
    }))
    .expect("valid question");

    let Question::Noul {
        criteria: Some(criteria),
        ..
    } = question
    else {
        panic!("expected Noul criteria");
    };
    assert_eq!(criteria.when_true, json!("matches"));
    assert_eq!(criteria.when_false, Value::Null);
}

#[test]
fn decodes_typed_answers_structured_legends_and_optional_usage() {
    let response: Response = serde_json::from_value(json!({
        "model": "jev-latest",
        "answers": {
            "yes": {"type": "noul", "noul": 0.8},
            "pick": {
                "type": "choice",
                "choice": "a",
                "probabilities": {"a": 0.7, "b": 0.3},
                "confidence": 0.6
            },
            "rate": {
                "type": "score",
                "score": 1.25,
                "legend": {"0": {"label": "low"}, "1": ["high"]},
                "probabilities": {"0": 0.25, "1": 0.75},
                "confidence": 0.5
            }
        },
        "usage": {"input_tokens": 12}
    }))
    .expect("valid response");

    assert_eq!(response.answer("yes").and_then(Answer::noul), Some(0.8));
    assert_eq!(response.answer("pick").and_then(Answer::choice), Some("a"));
    assert_eq!(response.answer("rate").and_then(Answer::score), Some(1.25));
    assert_eq!(response.usage.expect("usage").input_tokens, Some(12));
    assert_eq!(response.usage.expect("usage").output_tokens, None);
    let Some(Answer::Score { legend, .. }) = response.answer("rate") else {
        panic!("expected Score answer");
    };
    assert_eq!(legend["0"], json!({"label": "low"}));
}

#[test]
fn validation_error_identifies_the_field() {
    let error = Request::new(true).validate().expect_err("invalid state");
    assert!(matches!(error, Error::Validation { ref field, .. } if field == "state"));
}
