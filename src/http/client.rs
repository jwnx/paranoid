//! Generic HTTP client for fetching remote resources.

use std::io::Read;

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
}

/// Generic HTTP client for fetching remote resources.
#[derive(Debug, Clone)]
pub struct Client {
    inner: reqwest::Client,
}

impl Client {
    /// Create a new HTTP client.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be built.
    pub fn new() -> Result<Self, ClientError> {
        Ok(Self {
            inner: reqwest::Client::builder()
                .user_agent(concat!("paranoid/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(ClientError::Build)?,
        })
    }

    /// Send a GET request and return a validated response.
    ///
    /// Handles 404 specially (returns `ClientError::NotFound`) and converts
    /// other HTTP errors via `error_for_status()`.
    async fn fetch_response(&self, url: &str) -> Result<reqwest::Response, ClientError> {
        let response = self.inner.get(url).send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ClientError::NotFound {
                url: url.to_string(),
            });
        }

        Ok(response.error_for_status()?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let _client = Client::new().expect("Should create client");
    }
}
