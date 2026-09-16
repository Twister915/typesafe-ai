use std::collections::BTreeMap;

use serde_json::json;
use typesafe_ai::{Question, Request};

pub fn request() -> Request {
    Request::new("Help! My payouts have been failing for three days.")
        .with_question(
            "is_urgent",
            Question::noul("Does the customer convey urgency?"),
        )
        .with_question(
            "department",
            Question::choice(
                "Which team should handle this?",
                BTreeMap::from([
                    ("billing".into(), json!("Payments, invoices, and refunds")),
                    ("technical".into(), json!("Bugs, outages, and integrations")),
                    ("sales".into(), json!("Pricing, upgrades, and new accounts")),
                ]),
            ),
        )
        .with_question(
            "frustration",
            Question::score(
                "How frustrated is the customer?",
                vec![json!("Calm"), json!("Frustrated"), json!("Very angry")],
            ),
        )
}

pub fn print_answers(response: &typesafe_ai::Response) {
    if let Some(probability) = response
        .answer("is_urgent")
        .and_then(|answer| answer.noul())
    {
        println!("urgency probability: {probability:.2}");
    }
    if let Some(choice) = response
        .answer("department")
        .and_then(|answer| answer.choice())
    {
        println!("department: {choice}");
    }
    if let Some(score) = response
        .answer("frustration")
        .and_then(|answer| answer.score())
    {
        println!("frustration score: {score:.2}");
    }
}
