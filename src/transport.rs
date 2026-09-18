use std::time::{Duration, SystemTime};

use http::header::{HeaderValue, RETRY_AFTER};
use http::{HeaderMap, StatusCode};
use url::Url;

use crate::{ClientConfig, Error, ModelsResponse, Response};
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const RETRY_AFTER_MS: &str = "retry-after-ms";

#[derive(Clone)]
pub(crate) struct ValidatedConfig {
    pub(crate) endpoint: Url,
    pub(crate) models_endpoint: Url,
    pub(crate) authorization: HeaderValue,
    pub(crate) max_retries: u32,
    pub(crate) timeout: Duration,
}

impl ValidatedConfig {
    pub(crate) fn new(api_key: String, config: ClientConfig) -> Result<Self, Error> {
        if api_key.trim().is_empty() {
            return Err(Error::Configuration("API key cannot be empty".to_owned()));
        }
        if config.timeout.is_zero() {
            return Err(Error::Configuration(
                "per-attempt timeout must be greater than zero".to_owned(),
            ));
        }

        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| Error::Configuration("API key is not a valid header value".to_owned()))?;
        authorization.set_sensitive(true);

        Ok(Self {
            endpoint: endpoint(&config.base_url, "systemone")?,
            models_endpoint: endpoint(&config.base_url, "models")?,
            authorization,
            max_retries: config.max_retries,
            timeout: config.timeout,
        })
    }
}

pub(crate) struct RawResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

impl RawResponse {
    pub(crate) fn into_response<E>(self) -> Result<Response, Error<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let request_id = request_id(&self.headers);
        match serde_json::from_slice::<Response>(&self.body) {
            Ok(mut response) => {
                response.request_id = request_id;
                response.headers = self.headers;
                response.raw_body = self.body;
                Ok(response)
            }
            Err(source) => Err(Error::Decode {
                status: self.status,
                request_id,
                headers: Box::new(self.headers),
                body: self.body,
                source,
            }),
        }
    }

    pub(crate) fn into_models<E>(self) -> Result<ModelsResponse, Error<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let request_id = request_id(&self.headers);
        match serde_json::from_slice::<ModelsResponse>(&self.body) {
            Ok(mut response) => {
                response.request_id = request_id;
                response.headers = self.headers;
                response.raw_body = self.body;
                Ok(response)
            }
            Err(source) => Err(Error::Decode {
                status: self.status,
                request_id,
                headers: Box::new(self.headers),
                body: self.body,
                source,
            }),
        }
    }

    pub(crate) fn into_api_error<E>(self, attempts: u64) -> Error<E>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Error::Api {
            status: self.status,
            request_id: request_id(&self.headers),
            retry_after: retry_after(&self.headers),
            headers: Box::new(self.headers),
            body: self.body,
            attempts,
        }
    }
}

pub(crate) fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529
}

pub(crate) fn retry_delay(headers: &HeaderMap, retries: u32) -> Option<Duration> {
    let delay = backoff(retries).max(retry_after(headers).unwrap_or_default());
    (delay <= MAX_RETRY_DELAY).then_some(delay)
}

fn endpoint(base_url: &str, resource: &str) -> Result<Url, Error> {
    let mut url = Url::parse(base_url)
        .map_err(|error| Error::Configuration(format!("invalid base URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::Configuration(
            "base URL scheme must be http or https".to_owned(),
        ));
    }
    if url.host().is_none() {
        return Err(Error::Configuration(
            "base URL must include a host".to_owned(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Configuration(
            "base URL cannot contain username or password information".to_owned(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::Configuration(
            "base URL cannot contain a query or fragment".to_owned(),
        ));
    }
    url.path_segments_mut()
        .map_err(|()| Error::Configuration("base URL cannot contain path segments".to_owned()))?
        .pop_if_empty()
        .push("v1")
        .push(resource);
    Ok(url)
}

fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-typesafe-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let milliseconds = headers
        .get(RETRY_AFTER_MS)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis);

    let standard = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .parse::<u64>()
                .map(Duration::from_secs)
                .ok()
                .or_else(|| {
                    httpdate::parse_http_date(value)
                        .ok()
                        .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
                })
        });

    match (milliseconds, standard) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(delay), None) | (None, Some(delay)) => Some(delay),
        (None, None) => None,
    }
}

fn backoff(retries: u32) -> Duration {
    const BASE_MILLISECONDS: u64 = 250;
    const MAX_BACKOFF_MILLISECONDS: u64 = 8_000;

    let multiplier = 1_u64.checked_shl(retries.min(63)).unwrap_or(u64::MAX);
    Duration::from_millis(
        BASE_MILLISECONDS
            .saturating_mul(multiplier)
            .min(MAX_BACKOFF_MILLISECONDS),
    )
}

#[cfg(test)]
mod tests {
    use http::header::HeaderValue;
    use test_case::test_case;

    use super::*;

    #[test_case("0.9200"; "trailing zeroes")]
    #[test_case("9.2e-1"; "exponent notation")]
    #[test_case("0.123456789012345678901234567890"; "more digits than f64")]
    fn preserves_original_json_numbers(number: &str) {
        let body = format!(
            "{{\n  \"model\": \"jev-latest\", \"answers\": {{\"yes\": {{\"type\": \"noul\", \"noul\": {number}}}}}\n}}"
        )
        .into_bytes();
        let response = RawResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: body.clone(),
        }
        .into_response::<std::convert::Infallible>()
        .expect("valid response");

        assert_eq!(response.raw_body, body);
        assert!(
            response
                .answer("yes")
                .and_then(|answer| answer.noul())
                .is_some()
        );
    }

    #[test_case("retry-after", "2", Some(Duration::from_secs(2)); "retry after seconds")]
    #[test_case(RETRY_AFTER_MS, "1000", Some(Duration::from_secs(1)); "retry after milliseconds")]
    #[test_case("retry-after", "120", None; "oversized seconds")]
    #[test_case(RETRY_AFTER_MS, "61000", None; "oversized milliseconds")]
    fn parses_and_bounds_retry_delay(
        name: &'static str,
        value: &'static str,
        expected: Option<Duration>,
    ) {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        assert_eq!(retry_delay(&headers, 0), expected);
    }

    #[test]
    fn parses_and_bounds_retry_after_http_date() {
        let future = SystemTime::now() + Duration::from_secs(120);
        let value = httpdate::fmt_http_date(future);
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&value).expect("valid header"),
        );

        let parsed = retry_after(&headers).expect("date parses");
        assert!(parsed >= Duration::from_secs(118));
        assert!(parsed <= Duration::from_secs(120));
        assert_eq!(retry_delay(&headers, 0), None);
    }
}
