#![cfg(any(feature = "reqwest", feature = "ureq"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use typesafe_ai::{ClientConfig, Error, EvaluationEvent, Question, Request};

#[derive(Debug)]
struct ResponseSpec {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
    header_delay: Duration,
    body_delay: Duration,
}

impl ResponseSpec {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).expect("fixture serializes"),
            header_delay: Duration::ZERO,
            body_delay: Duration::ZERO,
        }
    }

    fn raw(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            header_delay: Duration::ZERO,
            body_delay: Duration::ZERO,
        }
    }

    fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    fn delay(mut self, delay: Duration) -> Self {
        self.header_delay = delay;
        self
    }

    fn body_delay(mut self, delay: Duration) -> Self {
        self.body_delay = delay;
        self
    }
}

#[derive(Debug)]
struct RecordedRequest {
    head: String,
    body: Vec<u8>,
}

struct MockServer {
    address: SocketAddr,
    requests: Receiver<RecordedRequest>,
    thread: Option<JoinHandle<()>>,
}

impl MockServer {
    fn start(responses: Vec<ResponseSpec>) -> Self {
        Self::start_observing(responses, Duration::ZERO)
    }

    fn start_observing(responses: Vec<ResponseSpec>, observe_after: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("mock address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            for response in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "timed out waiting for request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept request: {error}"),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .expect("blocking request stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("request read timeout");
                let request = read_request(&mut stream);
                sender.send(request).expect("record request");
                thread::sleep(response.header_delay);
                let reason = match response.status {
                    200 => "OK",
                    302 => "Found",
                    401 => "Unauthorized",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    529 => "Overloaded",
                    _ => "Fixture",
                };
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    reason,
                    response.body.len()
                );
                for (name, value) in response.headers {
                    head.push_str(name);
                    head.push_str(": ");
                    head.push_str(value);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                if stream.write_all(head.as_bytes()).is_ok() {
                    thread::sleep(response.body_delay);
                    let _ = stream.write_all(&response.body);
                }
            }
            let deadline = Instant::now() + observe_after;
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking request stream");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .expect("request read timeout");
                        let request = read_request(&mut stream);
                        sender.send(request).expect("record unexpected request");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("observe request: {error}"),
                }
            }
        });
        Self {
            address,
            requests,
            thread: Some(thread),
        }
    }

    fn base_url(&self, prefix: &str) -> String {
        format!("http://{}{prefix}", self.address)
    }

    fn finish(mut self) -> Vec<RecordedRequest> {
        self.thread
            .take()
            .expect("server thread")
            .join()
            .expect("server succeeds");
        self.requests.try_iter().collect()
    }
}

fn read_request(stream: &mut impl Read) -> RecordedRequest {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("read request");
        assert_ne!(read, 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec()).expect("ASCII request headers");
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .unwrap_or_default();
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).expect("read request body");
        assert_ne!(read, 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    RecordedRequest {
        head,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn request() -> Request {
    Request::new(json!({"message": "hello"}))
        .with_question("urgent", Question::noul("Is this urgent?"))
}

fn success() -> ResponseSpec {
    ResponseSpec::json(
        200,
        json!({
            "model": "jev-latest",
            "answers": {"urgent": {"type": "noul", "noul": 0.75}},
            "usage": {"input_tokens": 10, "output_tokens": 2}
        }),
    )
    .header("x-typesafe-request-id", "req_fixture")
}

fn config(server: &MockServer, prefix: &str) -> ClientConfig {
    ClientConfig {
        base_url: server.base_url(prefix),
        ..ClientConfig::default()
    }
}

fn assert_request(recorded: &RecordedRequest, expected_path: &str) {
    assert!(
        recorded
            .head
            .starts_with(&format!("POST {expected_path} HTTP/1.1"))
    );
    assert!(
        recorded
            .head
            .to_ascii_lowercase()
            .contains("authorization: bearer secret")
    );
    let body: Value = serde_json::from_slice(&recorded.body).expect("JSON request");
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["questions"]["urgent"]["type"], "noul");
}

fn assert_api_error<E>(error: Error<E>, status: u16, body: &[u8], attempts: u64) {
    assert_eq!(error.status().map(|status| status.as_u16()), Some(status));
    assert_eq!(error.request_id(), Some("req_error"));
    assert_eq!(error.body(), Some(body));
    assert_eq!(
        error
            .headers()
            .and_then(|headers| headers.get("x-extra"))
            .unwrap(),
        "kept"
    );
    assert!(matches!(error, Error::Api { attempts: value, .. } if value == attempts));
}

#[cfg(feature = "reqwest")]
mod async_client {
    use super::*;
    use http::HeaderValue;
    use std::task::{Context, Poll, Waker};
    use typesafe_ai::{ReqwestClient, Stream};

    #[tokio::test]
    async fn posts_to_prefixed_endpoint_and_decodes_metadata() {
        let fixture = success();
        let expected_body = fixture.body.clone();
        let server = MockServer::start(vec![fixture]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "/proxy")).expect("build client");

        let response = client.evaluate(&request()).await.expect("evaluate");
        assert_eq!(response.request_id.as_deref(), Some("req_fixture"));
        assert_eq!(response.raw_body, expected_body);
        assert_eq!(
            response.answer("urgent").and_then(|answer| answer.noul()),
            Some(0.75)
        );
        assert_eq!(response.usage.expect("usage").output_tokens, Some(2));
        let requests = server.finish();
        assert_request(&requests[0], "/proxy/v1/systemone");
    }

    #[tokio::test]
    async fn preserves_api_error_bytes_headers_and_attempt_count() {
        let body = vec![0xff, 0x00, b'e'];
        let server = MockServer::start(vec![
            ResponseSpec::raw(401, body.clone())
                .header("x-typesafe-request-id", "req_error")
                .header("x-extra", "kept"),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");

        let error = client.evaluate(&request()).await.expect_err("API error");
        assert!(!error.to_string().contains("\u{fffd}"));
        assert_api_error(error, 401, &body, 1);
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn retries_only_rate_limit_and_overload_responses() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "limited"),
            ResponseSpec::raw(529, "overloaded"),
            success(),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");

        let response = client.evaluate(&request()).await.expect("eventual success");
        assert_eq!(response.request_id.as_deref(), Some("req_fixture"));
        assert_eq!(server.finish().len(), 3);
    }

    #[tokio::test]
    async fn event_stream_is_lazy_and_fused_after_success() {
        let idle_server = MockServer::start(Vec::new());
        let idle_client =
            ReqwestClient::with_config("secret", config(&idle_server, "")).expect("build client");
        let idle_request = request();
        let events = idle_client.evaluate_events(&idle_request);
        drop(events);
        assert!(idle_server.finish().is_empty());

        let server = MockServer::start(vec![success()]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        let mut events = std::pin::pin!(client.evaluate_events(&request));
        assert!(matches!(
            std::future::poll_fn(|context| events.as_mut().poll_next(context)).await,
            Some(EvaluationEvent::Success(_))
        ));
        assert!(
            std::future::poll_fn(|context| events.as_mut().poll_next(context))
                .await
                .is_none()
        );
        assert!(
            std::future::poll_fn(|context| events.as_mut().poll_next(context))
                .await
                .is_none()
        );
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn event_stream_exposes_retry_and_counts_caller_pause() {
        let server = MockServer::start(vec![ResponseSpec::raw(429, "limited"), success()]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        let mut events = std::pin::pin!(client.evaluate_events(&request));

        let Some(EvaluationEvent::AttemptFailed(failure)) =
            std::future::poll_fn(|context| events.as_mut().poll_next(context)).await
        else {
            panic!("expected retryable failure");
        };
        assert_eq!(failure.attempt, 1);
        assert_eq!(failure.retry_delay, Some(Duration::from_millis(250)));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let resumed = Instant::now();
        assert!(matches!(
            std::future::poll_fn(|context| events.as_mut().poll_next(context)).await,
            Some(EvaluationEvent::Success(_))
        ));
        assert!(resumed.elapsed() < Duration::from_millis(200));
        assert_eq!(server.finish().len(), 2);
    }

    #[tokio::test]
    async fn dropping_a_stream_cancels_a_pending_retry() {
        let server = MockServer::start_observing(
            vec![ResponseSpec::raw(429, "limited")],
            Duration::from_millis(400),
        );
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        {
            let mut events = std::pin::pin!(client.evaluate_events(&request));
            assert!(matches!(
                std::future::poll_fn(|context| events.as_mut().poll_next(context)).await,
                Some(EvaluationEvent::AttemptFailed(_))
            ));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                events.as_mut().poll_next(&mut context),
                Poll::Pending
            ));
        }
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn dropping_a_stream_cancels_an_in_flight_request_future() {
        let server = MockServer::start_observing(
            vec![success().delay(Duration::from_millis(200))],
            Duration::from_millis(100),
        );
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        {
            let mut events = std::pin::pin!(client.evaluate_events(&request));
            let pending = tokio::time::timeout(
                Duration::from_millis(50),
                std::future::poll_fn(|context| events.as_mut().poll_next(context)),
            )
            .await;
            assert!(
                pending.is_err(),
                "request should still be awaiting its response"
            );
        }
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn exhausted_retry_events_end_with_a_terminal_failure() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "first"),
            ResponseSpec::raw(529, "second"),
            ResponseSpec::raw(429, "third"),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        let mut events = std::pin::pin!(client.evaluate_events(&request));

        for (attempt, retrying) in [(1, true), (2, true), (3, false)] {
            let Some(EvaluationEvent::AttemptFailed(failure)) =
                std::future::poll_fn(|context| events.as_mut().poll_next(context)).await
            else {
                panic!("expected failure event");
            };
            assert_eq!(failure.attempt, attempt);
            assert_eq!(failure.retry_delay.is_some(), retrying);
        }
        assert!(
            std::future::poll_fn(|context| events.as_mut().poll_next(context))
                .await
                .is_none()
        );
        assert_eq!(server.finish().len(), 3);
    }

    #[tokio::test]
    async fn zero_retries_disables_retrying() {
        let server = MockServer::start(vec![ResponseSpec::raw(429, "limited")]);
        let mut config = config(&server, "");
        config.max_retries = 0;
        let client = ReqwestClient::with_config("secret", config).expect("build client");

        let error = client.evaluate(&request()).await.expect_err("rate limit");
        assert!(matches!(error, Error::Api { attempts: 1, .. }));
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn exhausted_retries_report_all_attempts() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "first"),
            ResponseSpec::raw(529, "second"),
            ResponseSpec::raw(429, "third"),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");

        let error = client
            .evaluate(&request())
            .await
            .expect_err("retry exhaustion");
        assert!(matches!(error, Error::Api { attempts: 3, .. }));
        assert_eq!(server.finish().len(), 3);
    }

    #[tokio::test]
    async fn validates_before_opening_a_connection() {
        let client = ReqwestClient::new("secret").expect("build client");
        let error = client
            .evaluate(&Request::new(true))
            .await
            .expect_err("validation");
        assert!(matches!(error, Error::Validation { .. }));
    }

    #[test_case::test_case(500 ; "server error")]
    #[test_case::test_case(302 ; "redirect")]
    #[tokio::test]
    async fn does_not_retry_other_statuses(status: u16) {
        let server = MockServer::start(vec![
            ResponseSpec::raw(status, "stop")
                .header("x-typesafe-request-id", "req_error")
                .header("x-extra", "kept"),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let error = client.evaluate(&request()).await.expect_err("API error");
        assert_api_error(error, status, b"stop", 1);
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn does_not_wait_less_than_oversized_retry_after() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "wait")
                .header("Retry-After", "120")
                .header("retry-after-ms", "1")
                .header("x-typesafe-request-id", "req_error")
                .header("x-extra", "kept"),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let started = Instant::now();
        let error = client
            .evaluate(&request())
            .await
            .expect_err("oversized retry delay");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            matches!(error, Error::Api { retry_after: Some(delay), .. } if delay == Duration::from_secs(120))
        );
        assert_eq!(server.finish().len(), 1);
    }

    #[tokio::test]
    async fn returns_decode_error_with_raw_body_and_request_id() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(200, "not json")
                .header("x-typesafe-request-id", "req_error")
                .header("x-extra", "kept"),
        ]);
        let client =
            ReqwestClient::with_config("secret", config(&server, "")).expect("build client");
        let error = client.evaluate(&request()).await.expect_err("decode error");
        assert!(matches!(error, Error::Decode { .. }));
        assert_eq!(error.body(), Some(b"not json".as_slice()));
        assert_eq!(error.request_id(), Some("req_error"));
        server.finish();
    }

    #[tokio::test]
    async fn enforces_complete_attempt_timeout() {
        let server = MockServer::start(vec![success().delay(Duration::from_millis(100))]);
        let mut config = config(&server, "");
        config.timeout = Duration::from_millis(20);
        let client = ReqwestClient::with_config("secret", config).expect("build client");
        let error = client.evaluate(&request()).await.expect_err("timeout");
        assert!(
            matches!(error, Error::Timeout { timeout } if timeout == Duration::from_millis(20))
        );
        server.finish();
    }

    #[tokio::test]
    async fn timeout_covers_a_stalled_response_body() {
        let server = MockServer::start(vec![success().body_delay(Duration::from_millis(100))]);
        let mut config = config(&server, "");
        config.timeout = Duration::from_millis(20);
        let client = ReqwestClient::with_config("secret", config).expect("build client");
        let error = client.evaluate(&request()).await.expect_err("timeout");
        assert!(matches!(error, Error::Timeout { .. }));
        server.finish();
    }

    #[tokio::test]
    async fn honors_an_injected_reqwest_client() {
        let server = MockServer::start(vec![success()]);
        let mut headers = http::HeaderMap::new();
        headers.insert("x-injected", HeaderValue::from_static("yes"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("HTTP client");
        let client = ReqwestClient::with_http_client("secret", config(&server, ""), http)
            .expect("build client");
        client.evaluate(&request()).await.expect("evaluate");
        let requests = server.finish();
        assert!(
            requests[0]
                .head
                .to_ascii_lowercase()
                .contains("x-injected: yes")
        );
    }

    #[test_case::test_case("ftp://example.com" ; "unsupported scheme")]
    #[test_case::test_case("http://" ; "missing host")]
    #[test_case::test_case("http://user:pass@example.com" ; "userinfo")]
    #[test_case::test_case("http://example.com?query=yes" ; "query")]
    fn rejects_ambiguous_base_urls(base_url: &str) {
        let config = ClientConfig {
            base_url: base_url.to_owned(),
            ..ClientConfig::default()
        };
        assert!(matches!(
            ReqwestClient::with_config("secret", config),
            Err(Error::Configuration(_))
        ));
    }

    #[test]
    fn redacts_api_keys_from_debug_output() {
        let client = ReqwestClient::new("top-secret-key").expect("build client");
        let debug = format!("{client:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("top-secret-key"));
    }
}

#[cfg(feature = "ureq")]
mod blocking_client {
    use super::*;
    use typesafe_ai::UreqClient;

    #[test]
    fn posts_to_prefixed_endpoint_and_decodes_metadata() {
        let fixture = success();
        let expected_body = fixture.body.clone();
        let server = MockServer::start(vec![fixture]);
        let client =
            UreqClient::with_config("secret", config(&server, "/proxy")).expect("build client");

        let response = client.evaluate(&request()).expect("evaluate");
        assert_eq!(response.request_id.as_deref(), Some("req_fixture"));
        assert_eq!(
            response.answer("urgent").and_then(|answer| answer.noul()),
            Some(0.75)
        );
        assert_eq!(response.raw_body, expected_body);
        let requests = server.finish();
        assert_request(&requests[0], "/proxy/v1/systemone");
    }

    #[test]
    fn preserves_api_error_bytes_headers_and_attempt_count() {
        let body = vec![0xff, 0x00, b'e'];
        let server = MockServer::start(vec![
            ResponseSpec::raw(401, body.clone())
                .header("x-typesafe-request-id", "req_error")
                .header("x-extra", "kept"),
        ]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");

        let error = client.evaluate(&request()).expect_err("API error");
        assert_api_error(error, 401, &body, 1);
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn retries_rate_limit_and_overload_responses() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "limited"),
            ResponseSpec::raw(529, "overloaded"),
            success(),
        ]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");
        let response = client.evaluate(&request()).expect("eventual success");
        assert_eq!(response.request_id.as_deref(), Some("req_fixture"));
        assert_eq!(server.finish().len(), 3);
    }

    #[test]
    fn event_iterator_is_lazy_and_fused_after_success() {
        let idle_server = MockServer::start(Vec::new());
        let idle_client =
            UreqClient::with_config("secret", config(&idle_server, "")).expect("build client");
        let idle_request = request();
        let events = idle_client.evaluate_events(&idle_request);
        drop(events);
        assert!(idle_server.finish().is_empty());

        let server = MockServer::start(vec![success()]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        let mut events = client.evaluate_events(&request);
        assert!(matches!(events.next(), Some(EvaluationEvent::Success(_))));
        assert!(events.next().is_none());
        assert!(events.next().is_none());
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn event_iterator_exposes_retry_and_counts_caller_pause() {
        let server = MockServer::start(vec![ResponseSpec::raw(429, "limited"), success()]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        let mut events = client.evaluate_events(&request);

        let Some(EvaluationEvent::AttemptFailed(failure)) = events.next() else {
            panic!("expected retryable failure");
        };
        assert_eq!(failure.attempt, 1);
        assert_eq!(failure.retry_delay, Some(Duration::from_millis(250)));
        thread::sleep(Duration::from_millis(300));
        let resumed = Instant::now();
        assert!(matches!(events.next(), Some(EvaluationEvent::Success(_))));
        assert!(resumed.elapsed() < Duration::from_millis(200));
        assert_eq!(server.finish().len(), 2);
    }

    #[test]
    fn exhausted_retry_events_end_with_a_terminal_failure() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "first"),
            ResponseSpec::raw(529, "second"),
            ResponseSpec::raw(429, "third"),
        ]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");
        let request = request();
        let mut events = client.evaluate_events(&request);

        for (attempt, retrying) in [(1, true), (2, true), (3, false)] {
            let Some(EvaluationEvent::AttemptFailed(failure)) = events.next() else {
                panic!("expected failure event");
            };
            assert_eq!(failure.attempt, attempt);
            assert_eq!(failure.retry_delay.is_some(), retrying);
        }
        assert!(events.next().is_none());
        assert_eq!(server.finish().len(), 3);
    }

    #[test]
    fn zero_retries_disables_retrying() {
        let server = MockServer::start(vec![ResponseSpec::raw(429, "limited")]);
        let mut config = config(&server, "");
        config.max_retries = 0;
        let client = UreqClient::with_config("secret", config).expect("build client");

        let error = client.evaluate(&request()).expect_err("rate limit");
        assert!(matches!(error, Error::Api { attempts: 1, .. }));
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn exhausted_retries_report_all_attempts() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(429, "first"),
            ResponseSpec::raw(529, "second"),
            ResponseSpec::raw(429, "third"),
        ]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");

        let error = client.evaluate(&request()).expect_err("retry exhaustion");
        assert!(matches!(error, Error::Api { attempts: 3, .. }));
        assert_eq!(server.finish().len(), 3);
    }

    #[test]
    fn validates_before_opening_a_connection() {
        let client = UreqClient::new("secret").expect("build client");
        let error = client
            .evaluate(&Request::new(true))
            .expect_err("validation");
        assert!(matches!(error, Error::Validation { .. }));
    }

    #[test]
    fn does_not_retry_other_statuses() {
        let server = MockServer::start(vec![
            ResponseSpec::raw(500, "stop")
                .header("x-typesafe-request-id", "req_error")
                .header("x-extra", "kept"),
        ]);
        let client = UreqClient::with_config("secret", config(&server, "")).expect("build client");
        let error = client.evaluate(&request()).expect_err("API error");
        assert_api_error(error, 500, b"stop", 1);
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn enforces_complete_attempt_timeout() {
        let server = MockServer::start(vec![success().delay(Duration::from_millis(100))]);
        let mut config = config(&server, "");
        config.timeout = Duration::from_millis(20);
        let client = UreqClient::with_config("secret", config).expect("build client");
        let error = client.evaluate(&request()).expect_err("timeout");
        assert!(
            matches!(error, Error::Timeout { timeout } if timeout == Duration::from_millis(20))
        );
        server.finish();
    }

    #[test]
    fn timeout_covers_a_stalled_response_body() {
        let server = MockServer::start(vec![success().body_delay(Duration::from_millis(100))]);
        let mut config = config(&server, "");
        config.timeout = Duration::from_millis(20);
        let client = UreqClient::with_config("secret", config).expect("build client");
        let error = client.evaluate(&request()).expect_err("timeout");
        assert!(matches!(error, Error::Timeout { .. }));
        server.finish();
    }

    #[test]
    fn honors_an_injected_ureq_agent() {
        let server = MockServer::start(vec![success()]);
        let agent_config = ureq::Agent::config_builder()
            .user_agent("fixture-agent")
            .build();
        let agent = ureq::Agent::new_with_config(agent_config);
        let client =
            UreqClient::with_agent("secret", config(&server, ""), agent).expect("build client");
        client.evaluate(&request()).expect("evaluate");
        let requests = server.finish();
        assert!(
            requests[0]
                .head
                .to_ascii_lowercase()
                .contains("user-agent: fixture-agent")
        );
    }

    #[test]
    fn redacts_api_keys_from_debug_output() {
        let client = UreqClient::new("top-secret-key").expect("build client");
        let debug = format!("{client:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("top-secret-key"));
    }
}
