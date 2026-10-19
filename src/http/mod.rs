mod client;
#[cfg(test)]
pub(crate) mod test_server;

pub use client::{Client, ClientError, Retry};
