//! AWS Signature Version 4 for S3 requests, enough to `HEAD` and `PUT`
//! objects in an S3-compatible bucket (AWS, Tigris, MinIO, R2, ...).
//!
//! See <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use sha2::{Digest, Sha256};
use url::Url;

/// What a request needs to be signed for S3.
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub region: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// The SHA-256 of an empty payload, for requests without a body.
pub const EMPTY_PAYLOAD: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Everything but the unreserved characters gets percent-encoded.
const RESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

impl Credentials {
    /// Credentials from the variables the AWS tools read:
    /// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optionally
    /// `AWS_SESSION_TOKEN` and `AWS_REGION` (or `AWS_DEFAULT_REGION`;
    /// `us-east-1` without either). `None` unless both keys are set.
    pub fn from_env() -> Option<Self> {
        let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        Some(Self {
            access_key_id: var("AWS_ACCESS_KEY_ID")?,
            secret_access_key: var("AWS_SECRET_ACCESS_KEY")?,
            session_token: var("AWS_SESSION_TOKEN"),
            region: var("AWS_REGION")
                .or_else(|| var("AWS_DEFAULT_REGION"))
                .unwrap_or_else(|| "us-east-1".to_string()),
        })
    }

    /// The headers that sign a request for `url`, `Authorization`
    /// included. `payload_sha256` is the body's SHA-256 in hex (S3 checks
    /// the body against it), and `headers` are other headers to sign, which
    /// the caller sends too.
    pub fn sign(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        payload_sha256: &str,
        now: DateTime<Utc>,
    ) -> Vec<(String, String)> {
        let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = &timestamp[..8];
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };

        let mut added = vec![
            (
                "x-amz-content-sha256".to_string(),
                payload_sha256.to_string(),
            ),
            ("x-amz-date".to_string(), timestamp.clone()),
        ];
        if let Some(token) = &self.session_token {
            added.push(("x-amz-security-token".to_string(), token.clone()));
        }

        let mut signed: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
            .chain(std::iter::once(("host".to_string(), host)))
            .chain(added.iter().cloned())
            .collect();
        signed.sort();
        let names = signed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");

        let canonical_request = [
            method,
            &canonical_path(url),
            &canonical_query(url),
            &signed
                .iter()
                .map(|(name, value)| format!("{name}:{value}\n"))
                .collect::<String>(),
            &names,
            payload_sha256,
        ]
        .join("\n");

        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let string_to_sign = [
            "AWS4-HMAC-SHA256",
            &timestamp,
            &scope,
            &hex::encode(Sha256::digest(canonical_request.as_bytes())),
        ]
        .join("\n");

        let key = [date, &self.region, "s3", "aws4_request"].iter().fold(
            format!("AWS4{}", self.secret_access_key).into_bytes(),
            |key, part| hmac(&key, part.as_bytes()),
        );
        let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));

        added.push((
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
                self.access_key_id
            ),
        ));
        added
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes keys of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// The path with every segment encoded once, the way S3 expects it.
fn canonical_path(url: &Url) -> String {
    url.path()
        .split('/')
        .map(|segment| {
            let decoded = percent_decode_str(segment).decode_utf8_lossy();
            utf8_percent_encode(&decoded, RESERVED).to_string()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The query's parameters encoded and sorted.
fn canonical_query(url: &Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            (
                utf8_percent_encode(&k, RESERVED).to_string(),
                utf8_percent_encode(&v, RESERVED).to_string(),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example credentials in AWS's documentation.
    fn example() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
        }
    }

    fn may_24_2013() -> DateTime<Utc> {
        "2013-05-24T00:00:00Z".parse().unwrap()
    }

    fn authorization(headers: &[(String, String)]) -> &str {
        &headers
            .iter()
            .find(|(n, _)| n == "authorization")
            .unwrap()
            .1
    }

    // The examples in "Signature Calculations for the Authorization Header",
    // with the signatures AWS gives.

    #[test]
    fn signs_aws_get_object_example() {
        let url = "https://examplebucket.s3.amazonaws.com/test.txt"
            .parse()
            .unwrap();
        let headers = example().sign(
            "GET",
            &url,
            &[("Range", "bytes=0-9")],
            EMPTY_PAYLOAD,
            may_24_2013(),
        );
        assert_eq!(
            authorization(&headers),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn signs_aws_put_object_example() {
        let url = "https://examplebucket.s3.amazonaws.com/test$file.text"
            .parse()
            .unwrap();
        let payload = hex::encode(Sha256::digest(b"Welcome to Amazon S3."));
        let headers = example().sign(
            "PUT",
            &url,
            &[
                ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
                ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ],
            &payload,
            may_24_2013(),
        );
        assert!(
            authorization(&headers).ends_with(
                "Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
            ),
            "{}",
            authorization(&headers)
        );
        let names: Vec<&str> = headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["x-amz-content-sha256", "x-amz-date", "authorization"]
        );
    }

    #[test]
    fn signs_the_port_and_the_session_token() {
        let mut credentials = example();
        credentials.session_token = Some("token".to_string());
        let url = "http://127.0.0.1:9000/bucket/key".parse().unwrap();
        let headers = credentials.sign("HEAD", &url, &[], EMPTY_PAYLOAD, may_24_2013());
        assert!(
            authorization(&headers).contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token,"
            )
        );
        assert!(headers.contains(&("x-amz-security-token".to_string(), "token".to_string())));
    }

    #[test]
    fn encodes_the_path_once() {
        let url: Url = "https://b.example.org/a%20b/c$d/e~f".parse().unwrap();
        assert_eq!(canonical_path(&url), "/a%20b/c%24d/e~f");
    }

    #[test]
    fn keeps_secrets_out_of_debug() {
        let debug = format!("{:?}", example());
        assert!(!debug.contains("EXAMPLEKEY"), "{debug}");
    }
}
