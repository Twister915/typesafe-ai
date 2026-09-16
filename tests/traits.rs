use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use serde_json::json;
use test_case::test_case;
use typesafe_ai::{
    AsyncClient, Error, EvaluationEvent, EvaluationFailure, Request, Response, Stream, SyncClient,
};

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Success,
    Failure,
    Incomplete,
}

/// An application-supplied client needs no HTTP backend or async runtime.
#[derive(Debug)]
struct FixtureClient(Outcome);

fn fixture_events(request: &Request, outcome: Outcome) -> Vec<EvaluationEvent<io::Error>> {
    let mut events = vec![EvaluationEvent::Failed(EvaluationFailure {
        error: Error::Transport(io::Error::other("temporary failure")),
        attempt: 1,
        retry_delay: Some(Duration::ZERO),
    })];
    match outcome {
        Outcome::Success => events.push(EvaluationEvent::Success(
            serde_json::from_value(json!({
                "model": request.model,
                "answers": {"urgent": {"type": "noul", "noul": 0.9}}
            }))
            .expect("valid response fixture"),
        )),
        Outcome::Failure => events.push(EvaluationEvent::Failed(EvaluationFailure {
            error: Error::Transport(io::Error::other("terminal failure")),
            attempt: 2,
            retry_delay: None,
        })),
        Outcome::Incomplete => {}
    }
    events
}

impl SyncClient for FixtureClient {
    type TransportError = io::Error;

    fn evaluate_events(
        &self,
        request: &Request,
    ) -> impl Iterator<Item = EvaluationEvent<Self::TransportError>> {
        fixture_events(request, self.0).into_iter()
    }
}

#[derive(Debug)]
struct ReadyEvents(std::vec::IntoIter<EvaluationEvent<io::Error>>);

impl Stream for ReadyEvents {
    type Item = EvaluationEvent<io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next())
    }
}

impl AsyncClient for FixtureClient {
    type TransportError = io::Error;

    fn evaluate_events<'a>(
        &'a self,
        request: &'a Request,
    ) -> impl Stream<Item = EvaluationEvent<Self::TransportError>> + Send + 'a {
        ReadyEvents(fixture_events(request, self.0).into_iter())
    }
}

fn evaluate_sync<C>(client: &C, request: &Request) -> Result<Response, Error<C::TransportError>>
where
    C: SyncClient,
{
    client.evaluate(request)
}

fn evaluate_async<'a, C>(
    client: &'a C,
    request: &'a Request,
) -> impl Future<Output = Result<Response, Error<C::TransportError>>> + Send + 'a
where
    C: AsyncClient,
{
    client.evaluate(request)
}

fn assert_result(result: Result<Response, Error<io::Error>>, outcome: Outcome) {
    match outcome {
        Outcome::Success => {
            let response = result.expect("fixture succeeds after retry");
            assert_eq!(response.model, "fixture-model");
            assert_eq!(
                response.answer("urgent").and_then(|answer| answer.noul()),
                Some(0.9)
            );
        }
        Outcome::Failure => {
            let Error::Transport(source) = result.expect_err("terminal failure") else {
                panic!("the source should retain its concrete transport type");
            };
            assert_eq!(source.kind(), io::ErrorKind::Other);
            assert_eq!(source.to_string(), "terminal failure");
        }
        Outcome::Incomplete => {
            assert!(matches!(result, Err(Error::IncompleteEvaluation)));
        }
    }
}

#[test_case(Outcome::Success; "retry then success")]
#[test_case(Outcome::Failure; "terminal transport failure")]
#[test_case(Outcome::Incomplete; "producer stopped before terminal event")]
fn default_sync_evaluate_drives_custom_events(outcome: Outcome) {
    let request = Request::new("Please help").with_model("fixture-model");
    assert_result(evaluate_sync(&FixtureClient(outcome), &request), outcome);
}

#[test_case(Outcome::Success; "retry then success")]
#[test_case(Outcome::Failure; "terminal transport failure")]
#[test_case(Outcome::Incomplete; "producer stopped before terminal event")]
fn default_async_evaluate_is_send_and_drives_custom_events(outcome: Outcome) {
    let request = Request::new("Please help").with_model("fixture-model");
    let client = FixtureClient(outcome);
    let future = evaluate_async(&client, &request);
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());

    let Poll::Ready(result) = future.as_mut().poll(&mut context) else {
        panic!("fixture events are immediately ready");
    };
    assert_result(result, outcome);
}

#[test]
fn failure_events_expose_retry_and_terminal_status() {
    let request = Request::new("Please help");
    let events: Vec<_> =
        SyncClient::evaluate_events(&FixtureClient(Outcome::Failure), &request).collect();

    assert_eq!(events.len(), 2);
    assert!(!events[0].is_terminal());
    assert!(events[1].is_terminal());
}
