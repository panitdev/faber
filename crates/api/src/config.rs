use std::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub surge_url: String,
    /// Required for the remote provider; absent is only valid under the test provider,
    /// which talks to no server. `build_auth_provider` enforces that.
    pub surge_service_token: Option<String>,
    pub surge_cookie_domain: String,
    /// Origin serving the auth UI, used purely to build a CORS zone: under the
    /// remote provider it is the sole allowed origin for the credential-entry
    /// routes. It produces no redirect on this hop — upstream owns the handoff to
    /// the auth UI, and answers `GET /v1/login` with its own redirect. Faber's
    /// deployment does not exercise that zone either, since the auth UI posts
    /// credentials straight to surge-server rather than back through this proxy.
    pub surge_auth_ui_origin: String,
    /// Session lifetime advertised by the proxied perimeter. Matches upstream's.
    pub surge_session_ttl: Duration,
    pub api_port: u16,
    pub cors_origins: Vec<String>,
    /// Whether users may register hosts reached through the API process itself.
    /// Existing local hosts are left intact when this is disabled.
    pub allow_local_hosts: bool,
    /// This API's own externally reachable base URL — what a machine *out
    /// there* dials to get here. Nothing else in this struct answers that:
    /// `surge_url` points at the auth service, and `cors_origins` names
    /// browsers allowed to call in, neither of which is where an agent
    /// daemon on someone else's infrastructure should send its traffic.
    ///
    /// Only agent enrollment needs it, and only to hand a copy-pasteable
    /// install command to a machine faber cannot see. Unset is not an error
    /// at boot — every other route works without it — but it makes that one
    /// command unservable, which is why it is an `Option` rather than a
    /// default that would be silently wrong.
    pub public_url: Option<String>,
    /// Host suffix used by dormant live-preview URL construction. Production
    /// deployments set this to a separate registrable domain.
    pub preview_domain: String,
    /// `http` for local development; production preview domains use `https`.
    pub preview_scheme: String,
    pub preview_connect_timeout: Duration,
    pub preview_header_timeout: Duration,
    /// Where the agent daemon binaries served to enrolling hosts live, one
    /// file per architecture. The image bakes them in; a dev checkout has
    /// them wherever cargo put them.
    pub agent_binary_dir: PathBuf,
    /// One SearXNG instance to search through. Set it and every run gets the
    /// `search` tool; leave it unset and no run does.
    pub searxng_url: Option<String>,
    /// Parallel Search API key. Used when no named SearXNG instance is set.
    pub parallel_api_key: Option<String>,
    /// Search the public SearXNG network instead of one named instance.
    /// Ignored when `searxng_url` is set — an instance the operator named is
    /// a more specific answer than a pool discovered at boot.
    pub search_public_network: bool,
    /// Outbound proxy for search traffic, and for search traffic only. Passed
    /// explicitly because the search crate refuses to read `HTTPS_PROXY` from
    /// the process: this is a multi-user service, and nothing about one user's
    /// run may be decided by the host's ambient configuration.
    pub search_proxy: Option<String>,
    /// Where the read-only model presets are fetched from at boot. Defaults
    /// to the AI Model Directory's `all.min.json`; a deployment can mirror it
    /// locally. A fetch failure is logged and served as "presets unavailable"
    /// rather than taking the service down — see `crates/presets`.
    pub model_directory_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            surge_url: env::var("SURGE_URL").unwrap_or_else(|_| "http://localhost:3000".to_owned()),
            surge_service_token: env::var("SURGE_SERVICE_TOKEN").ok(),
            surge_cookie_domain: env::var("SURGE_COOKIE_DOMAIN")
                .unwrap_or_else(|_| ".panit.dev".to_owned()),
            surge_auth_ui_origin: env::var("SURGE_AUTH_UI_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:3000".to_owned()),
            surge_session_ttl: env::var("SURGE_SESSION_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(72 * 3600)),
            api_port: env::var("API_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3001),
            cors_origins: env::var("CORS_ORIGIN")
                .ok()
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            allow_local_hosts: env::var("FABER_ALLOW_LOCAL_HOSTS")
                .map(|value| value.trim() != "false")
                .unwrap_or(true),
            public_url: env::var("FABER_PUBLIC_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_owned())
                .filter(|value| !value.is_empty()),
            preview_domain: env::var("FABER_PREVIEW_DOMAIN")
                .unwrap_or_else(|_| "localhost".to_owned())
                .trim()
                .trim_start_matches('.')
                .to_owned(),
            preview_scheme: env::var("FABER_PREVIEW_SCHEME").unwrap_or_else(|_| "http".to_owned()),
            preview_connect_timeout: env::var("FABER_PREVIEW_CONNECT_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(10)),
            preview_header_timeout: env::var("FABER_PREVIEW_HEADER_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(30)),
            // The image copies the binaries here; a dev checkout falls back
            // to cargo's release directory, so `cargo run` can serve an
            // installer script built from a local `cargo build --release -p
            // faber-agent` without any further setup.
            agent_binary_dir: env::var("FABER_AGENT_BINARY_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("target/release")),
            searxng_url: env::var("SEARXNG_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            parallel_api_key: env::var("PARALLEL_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            search_public_network: env::var("SEARCH_PUBLIC_NETWORK")
                .map(|value| value == "true")
                .unwrap_or(false),
            search_proxy: env::var("SEARCH_PROXY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            model_directory_url: env::var("FABER_MODEL_DIRECTORY_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| presets::DEFAULT_URL.to_owned()),
        }
    }
}
