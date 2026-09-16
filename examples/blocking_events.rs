mod support;

use std::{env, error::Error};

use typesafe_ai::{EvaluationEvent, UreqClient};

fn main() -> Result<(), Box<dyn Error>> {
    let client = UreqClient::new(env::var("TYPESAFE_API_KEY")?)?;
    let request = support::request();

    for event in client.evaluate_events(&request) {
        match event {
            EvaluationEvent::Failed(failure) => {
                eprintln!("attempt {} failed: {}", failure.attempt, failure.error);
                if let Some(delay) = failure.retry_delay {
                    eprintln!("retrying in {delay:?}");
                } else {
                    return Err(failure.error.into());
                }
            }
            EvaluationEvent::Success(response) => {
                support::print_answers(&response);
            }
        }
    }

    Ok(())
}
