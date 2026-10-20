mod client;
mod sigv4;
#[cfg(test)]
pub(crate) mod test_server;

pub use client::{Client, ClientError, Head, Retry};
pub use sigv4::{Credentials, EMPTY_PAYLOAD};
