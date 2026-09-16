mod support;

use std::env;
use std::error::Error;
use std::future::poll_fn;
use std::pin::pin;

use typesafe_ai::{EvaluationEvent, ReqwestClient, Stream};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = ReqwestClient::new(env::var("TYPESAFE_API_KEY")?)?;
    let request = support::request();
    let events = client.evaluate_events(&request);
    let mut events = pin!(events);

    while let Some(event) = poll_fn(|context| events.as_mut().poll_next(context)).await {
        match event {
            EvaluationEvent::AttemptFailed(failure) => {
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
