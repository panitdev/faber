//! Getting the directory over HTTP.
//!
//! Separate from [`crate::Catalog::parse`] so the parse is testable without a
//! network, and separate from the service that calls it so the crate keeps no
//! opinion about when a fetch happens or what to do when it fails.

use std::time::Duration;

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
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("faber-presets/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Self::fetch_with(&client, url).await
    }

    /// Fetches through a caller's client — for a service that already holds
    /// one configured the way it wants.
    pub async fn fetch_with(client: &reqwest::Client, url: &str) -> Result<Self, Error> {
        let bytes = client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        Self::parse(&bytes)
    }
}
