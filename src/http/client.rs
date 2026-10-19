//! Generic HTTP client for fetching remote resources.

use std::future::Future;
use std::io::Read;
use std::num::NonZeroU32;
use std::time::Duration;

use flate2::read::GzDecoder;
use thiserror::Error;
use xz2::read::XzDecoder;

/// Errors that can occur during HTTP operations.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("failed to build HTTP client: {0}")]
    Build(reqwest::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("decompression error: {0}")]
    Decompress(std::io::Error),

    #[error("not found: {url}")]
    NotFound { url: String },

    #[error("{url} answered {status}")]
    Status {
        url: String,
        status: reqwest::StatusCode,
    },
}

impl ClientError {
    /// Whether asking again might work: the server was overloaded or
    /// failing, or the network was. A 404 or a 403 will say the same thing
    /// next time.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(e) => !e.is_builder() && !e.is_redirect(),
            Self::Status { status, .. } => {
                status.is_server_error()
                    || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || *status == reqwest::StatusCode::REQUEST_TIMEOUT
            }
            Self::Build(_) | Self::Decompress(_) | Self::NotFound { .. } => false,
        }
    }
}

/// How many times to try a request that failed for a transient reason
/// (see [`ClientError::is_transient`]), and how long to wait in between.
/// The wait doubles after each attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub attempts: NonZeroU32,
    pub backoff: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: NonZeroU32::new(3).unwrap(),
            backoff: Duration::from_millis(500),
        }
    }
}

impl Retry {
    /// Run `request` until it succeeds, fails for good, or runs out of
    /// attempts. Returns the last result and how many attempts it took.
    pub async fn run<T, F, Fut>(&self, mut request: F) -> (Result<T, ClientError>, u32)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ClientError>>,
    {
        let mut backoff = self.backoff;
        let mut attempt = 1;
        loop {
            let result = request().await;
            match &result {
                Err(e) if e.is_transient() && attempt < self.attempts.get() => {
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                    attempt += 1;
                }
                _ => return (result, attempt),
            }
        }
    }
}

/// Generic HTTP client for fetching remote resources.
#[derive(Debug, Clone)]
pub struct Client {
    inner: reqwest::Client,
}

impl Client {
    /// How long to wait for a connection.
    pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    /// How long to wait for the next bytes of a response. A slow download
    /// can take as long as it needs, as long as it doesn't stall.
    pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

    /// Create a new HTTP client.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be built.
    pub fn new() -> Result<Self, ClientError> {
        Self::with_timeouts(Self::CONNECT_TIMEOUT, Self::READ_TIMEOUT)
    }

    /// Create a client with its own connect and read timeouts.
    pub fn with_timeouts(connect: Duration, read: Duration) -> Result<Self, ClientError> {
        Ok(Self {
            inner: reqwest::Client::builder()
                .user_agent(concat!("paranoid/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(connect)
                .read_timeout(read)
                .build()
                .map_err(ClientError::Build)?,
        })
    }

    /// Send a GET request and return a successful response.
    ///
    /// A 404 is [`ClientError::NotFound`], since callers often have
    /// somewhere else to look, and any other failure status is
    /// [`ClientError::Status`].
    async fn fetch_response(&self, url: &str) -> Result<reqwest::Response, ClientError> {
        let response = self.inner.get(url).send().await?;
        check_status(url, response)
    }

    /// Fetch a URL and return the response body as bytes.
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, ClientError> {
        let response = self.fetch_response(url).await?;
        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// Decompress gzip data.
    pub fn decompress_gzip(data: &[u8]) -> Result<String, ClientError> {
        let mut decoder = GzDecoder::new(data);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .map_err(ClientError::Decompress)?;
        Ok(decompressed)
    }

    /// Decompress xz data.
    pub fn decompress_xz(data: &[u8]) -> Result<String, ClientError> {
        let mut decoder = XzDecoder::new(data);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .map_err(ClientError::Decompress)?;
        Ok(decompressed)
    }
}

/// Turn a failure status into an error.
fn check_status(url: &str, response: reqwest::Response) -> Result<reqwest::Response, ClientError> {
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(ClientError::NotFound {
            url: url.to_string(),
        });
    }
    if status.is_client_error() || status.is_server_error() {
        return Err(ClientError::Status {
            url: url.to_string(),
            status,
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::http::test_server::{Reply, TestServer};

    #[test]
    fn test_client_creation() {
        let _client = Client::new().expect("Should create client");
    }

    fn quick() -> Retry {
        Retry {
            attempts: NonZeroU32::new(3).unwrap(),
            backoff: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn classifies_failures() {
        let server = TestServer::start(|request| match request.path.as_str() {
            "/missing" => Reply::Status(404),
            "/forbidden" => Reply::Status(403),
            "/busy" => Reply::Status(429),
            "/broken" => Reply::Status(503),
            "/cut" => Reply::Close,
            _ => {
                let agent = request.headers.iter().find(|(n, _)| n == "user-agent");
                assert_eq!(
                    agent.unwrap().1,
                    concat!("paranoid/", env!("CARGO_PKG_VERSION"))
                );
                Reply::Body(b"ok".to_vec())
            }
        })
        .await;
        let client = Client::new().unwrap();
        let fetch = async |path: &str| client.fetch_bytes(&server.url(path)).await;

        assert_eq!(fetch("/ok").await.unwrap(), b"ok");
        let missing = fetch("/missing").await.unwrap_err();
        assert!(matches!(missing, ClientError::NotFound { .. }));
        assert!(!missing.is_transient());
        let forbidden = fetch("/forbidden").await.unwrap_err();
        assert_eq!(
            forbidden.to_string(),
            format!("{} answered 403 Forbidden", server.url("/forbidden"))
        );
        assert!(!forbidden.is_transient());
        assert!(fetch("/busy").await.unwrap_err().is_transient());
        assert!(fetch("/broken").await.unwrap_err().is_transient());
        assert!(fetch("/cut").await.unwrap_err().is_transient());
    }

    #[tokio::test]
    async fn a_stalled_response_times_out() {
        let server = TestServer::start(|_| Reply::Hang).await;
        let client =
            Client::with_timeouts(Duration::from_secs(1), Duration::from_millis(200)).unwrap();
        let error = client.fetch_bytes(&server.url("/")).await.unwrap_err();
        assert!(error.is_transient(), "{error:?}");
    }

    #[tokio::test]
    async fn retries_only_what_might_work_next_time() {
        let calls = AtomicU32::new(0);
        let (result, attempts) = quick()
            .run(|| async {
                calls.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>(ClientError::NotFound { url: "x".into() })
            })
            .await;
        assert!(result.is_err());
        assert_eq!((attempts, calls.load(Ordering::Relaxed)), (1, 1));

        let server = TestServer::start(|_| Reply::Status(503)).await;
        let client = Client::new().unwrap();
        let url = server.url("/");
        let (result, attempts) = quick().run(|| client.fetch_bytes(&url)).await;
        assert!(result.is_err());
        assert_eq!(attempts, 3);
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn stops_retrying_once_it_works() {
        let server = TestServer::start(|request| match request.count {
            1 => Reply::Status(502),
            _ => Reply::Body(b"ok".to_vec()),
        })
        .await;
        let client = Client::new().unwrap();
        let url = server.url("/");
        let (result, attempts) = quick().run(|| client.fetch_bytes(&url)).await;
        assert_eq!(result.unwrap(), b"ok");
        assert_eq!(attempts, 2);
    }
}
