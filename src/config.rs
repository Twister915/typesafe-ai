use std::time::Duration;

/// Configuration shared by the blocking and asynchronous HTTP clients.
///
/// Credentials are supplied separately when constructing a client.
///
/// ```
/// use std::time::Duration;
///
/// use typesafe_ai::ClientConfig;
///
/// let config = ClientConfig {
///     timeout: Duration::from_secs(20),
///     max_retries: 0,
///     ..ClientConfig::default()
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// API base URL.
    ///
    /// Defaults to `https://api.typesafe.ai`. Path prefixes are preserved when the
    /// `/v1/systemone` endpoint is appended.
    pub base_url: String,
    /// Timeout for each complete HTTP attempt, including its response body.
    ///
    /// Defaults to 60 seconds.
    pub timeout: Duration,
    /// Number of retries after the initial attempt.
    ///
    /// Defaults to two. Clients retry only HTTP 429 and 529 responses.
    pub max_retries: u32,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.typesafe.ai".to_owned(),
            timeout: Duration::from_secs(60),
            max_retries: 2,
        }
    }
}
