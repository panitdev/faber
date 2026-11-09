//! Getting the directory over HTTP.
//!
//! Separate from [`crate::Catalog::parse`] so the parse is testable without a
//! network, and separate from the service that calls it so the crate keeps no
//! opinion about when a fetch happens or what to do when it fails.

use std::io::Read;
use std::time::Duration;

use flate2::read::GzDecoder;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING};

use crate::{Catalog, Error};

impl Catalog {
    /// Fetches and parses the directory at `url`.
    ///
    /// Builds its own client rather than taking one, and that client reads no
    /// process environment: `search` makes the same choice for the same
    /// reason. A URL is a fixed, operator-supplied constant here, but the
    /// habit of letting `HTTPS_PROXY` decide where a service's traffic goes is
    /// not one to spread.
    pub async fn fetch(url: &str) -> Result<Self, Error> {
        // Idle timeouts rather than a total one: a slow link can take tens of
        // seconds to deliver the catalog while making steady progress. What
        // should fail is a connection that never opens or a transfer that
        // stalls. A caller with a deadline of its own applies it around this.
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .user_agent(concat!("faber-presets/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Self::fetch_with(&client, url).await
    }

    /// Fetches through a caller's client — for a service that already holds
    /// one configured the way it wants.
    ///
    /// Asks for gzip and decompresses here rather than through reqwest's
    /// `gzip` feature. Cargo unifies features across the build, so turning it
    /// on for this one download would also change the LLM streaming clients
    /// and the search fetcher. The catalog is about ten times smaller gzipped.
    pub async fn fetch_with(client: &reqwest::Client, url: &str) -> Result<Self, Error> {
        let response = client
            .get(url)
            .header(ACCEPT_ENCODING, "gzip")
            .send()
            .await?
            .error_for_status()?;
        let gzipped = response
            .headers()
            .get(CONTENT_ENCODING)
            .is_some_and(|encoding| encoding.as_bytes().eq_ignore_ascii_case(b"gzip"));
        let bytes = response.bytes().await?;
        if gzipped {
            Self::parse(&gunzip(&bytes)?)
        } else {
            Self::parse(&bytes)
        }
    }
}

fn gunzip(compressed: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut decompressed = Vec::new();
    GzDecoder::new(compressed).read_to_end(&mut decompressed)?;
    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    use super::gunzip;

    #[test]
    fn gunzip_restores_the_original_bytes() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(br#"{"providers":{}}"#).unwrap();
        let compressed = encoder.finish().unwrap();

        assert_eq!(gunzip(&compressed).unwrap(), br#"{"providers":{}}"#);
    }

    #[test]
    fn gunzip_rejects_a_body_that_is_not_gzip() {
        assert!(gunzip(b"{\"providers\":{}}").is_err());
    }
}
