use std::collections::BTreeMap;

use http::{HeaderMap, StatusCode};
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
    let result = Request::new(state)
        .with_question("q", Question::noul("check"))
        .validate();
    assert_eq!(result.is_ok(), valid);
}

#[test]
fn rejects_empty_questions() {
    let error = Request::new("state")
        .validate()
        .expect_err("a System One request needs at least one question");
    assert!(matches!(
        error,
        Error::Validation { ref field, ref message }
            if field == "questions" && message.contains("at least one question")
    ));
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

#[test]
fn documented_request_fixture_deserializes_and_validates() {
    let request: Request =
        serde_json::from_str(include_str!("fixtures/contract/systemone-request.json"))
            .expect("documented request fixture deserializes");

    request
        .validate()
        .expect("documented request fixture validates");
    assert_eq!(request.model, "jev-latest");
    assert_eq!(request.questions.len(), 3);
}

#[test]
fn omitted_instructions_fixture_deserializes_as_null() {
    let request: Request = serde_json::from_str(include_str!(
        "fixtures/contract/systemone-optional-instructions.json"
    ))
    .expect("optional instructions fixture deserializes");

    request
        .validate()
        .expect("optional instructions fixture validates");
    for question in request.questions.values() {
        let instructions = match question {
            Question::Noul { instructions, .. }
            | Question::Choice { instructions, .. }
            | Question::Score { instructions, .. } => instructions,
        };
        assert_eq!(instructions, &Value::Null);
    }
}

#[test]
fn documented_response_fixture_decodes() {
    let response: Response =
        serde_json::from_str(include_str!("fixtures/contract/systemone-response.json"))
            .expect("documented response fixture deserializes");

    assert_eq!(response.model, "jev-latest");
    assert_eq!(response.answers.len(), 3);
    assert_eq!(
        response.answer("is_urgent").and_then(Answer::noul),
        Some(0.92)
    );
    assert_eq!(response.usage.expect("usage").output_tokens, Some(48));
}

#[test]
fn api_error_details_are_structured_without_consuming_the_raw_body() {
    let body = br#"{
        "detail": [{
            "loc": ["body", "questions", "urgent"],
            "msg": "field required",
            "type": "missing"
        }]
    }"#
    .to_vec();
    let error: Error = Error::Api {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        request_id: Some("req_error".to_owned()),
        headers: Box::new(HeaderMap::new()),
        body: body.clone(),
        retry_after: None,
        attempts: 1,
    };

    let details = error
        .api_error_details()
        .expect("documented validation details");
    assert_eq!(details.validation.len(), 1);
    assert_eq!(details.validation[0].message, "field required");
    assert_eq!(error.body(), Some(body.as_slice()));
}

#[test]
fn api_error_details_return_none_for_unrecognized_bodies() {
    let body = b"not json".to_vec();
    let error: Error = Error::Api {
        status: StatusCode::BAD_GATEWAY,
        request_id: None,
        headers: Box::new(HeaderMap::new()),
        body: body.clone(),
        retry_after: None,
        attempts: 1,
    };

    assert_eq!(error.api_error_details(), None);
    assert_eq!(error.body(), Some(body.as_slice()));

    let local: Error = Error::Validation {
        field: "state".to_owned(),
        message: "invalid".to_owned(),
    };
    assert_eq!(local.api_error_details(), None);
}

#[test]
fn decode_error_keeps_raw_body_without_api_error_details() {
    let body = b"not a System One response".to_vec();
    let source = serde_json::from_slice::<Response>(&body).expect_err("invalid response JSON");
    let error: Error = Error::Decode {
        status: StatusCode::OK,
        request_id: None,
        headers: Box::new(HeaderMap::new()),
        body: body.clone(),
        source,
    };

    assert_eq!(error.api_error_details(), None);
    assert_eq!(error.body(), Some(body.as_slice()));
}
