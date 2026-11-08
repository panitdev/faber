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
    /// Where the model presets are fetched from at boot, before they are
    /// written to the database the browse routes read. Defaults to models.dev's
    /// `catalog.json`; a deployment can mirror it locally, and the mirror must
    /// keep that shape.
    /// A fetch failure is logged and the previously stored catalog is kept
    /// rather than taking the service down — see `crates/presets`.
    pub model_directory_url: String,
    /// What the scratch runtime runs on. Read here, once, and nowhere else:
    /// nothing about a user's sandbox may come from this process's ambient
    /// environment.
    pub scratch: ScratchSettings,
}

/// The operator's scratch settings. Scratch is unavailable unless both
/// `root` and `rootfs` are set.
#[derive(Clone, Debug, Default)]
pub struct ScratchSettings {
    /// `FABER_SCRATCH_ROOT`: where sessions' ephemeral layers live, and
    /// projects' volumes when there is no tenantd.
    pub root: Option<PathBuf>,
    /// `FABER_SCRATCH_ROOTFS`: the base image, exported to a directory, for
    /// virtual mode.
    pub rootfs: Option<PathBuf>,
    /// `FABER_SCRATCH_TOOLSTORE`: shown read-only at `/opt/toolstore` in
    /// virtual mode.
    pub toolstore: Option<PathBuf>,
    /// tenantd's API: `FABER_TENANTD_SOCKET`, its Unix socket on this host,
    /// or `FABER_TENANTD_ENDPOINT` with `FABER_TENANTD_TLS_CA`, `_CERT` and
    /// `_KEY` (and optionally `_SERVER_NAME`), its mTLS listener. Unset,
    /// scratch has no compute mode and a command that needs it fails, saying
    /// why.
    pub tenantd: Option<scratch::Endpoint>,
    /// `FABER_SCRATCH_IMAGE`: the scratch image as a registry reference, the
    /// same image `FABER_SCRATCH_ROOTFS` was exported from.
    pub image: Option<String>,
    /// `FABER_SCRATCH_PROFILE`: the tenantd profile scratch sandboxes run
    /// under, when not `scratch`.
    pub profile: Option<String>,
    /// `FABER_SCRATCH_QUOTA_BYTES`: what one user's scratches may hold in
    /// total, across all their sessions and projects. Unset is no limit in
    /// virtual mode, and tenantd's default for the user's tenant.
    pub quota_bytes: Option<u64>,
    /// Why these settings cannot be used as given. Scratch is then off, and
    /// the log says why, rather than running with a limit the operator did
    /// not mean or taking the server down at boot.
    pub invalid: Option<String>,
}

impl ScratchSettings {
    pub fn from_env() -> Self {
        ScratchSettings::from_vars(|name| env::var(name).ok())
    }

    /// The settings from whatever `var` answers for each variable name.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        let path = |name: &str| set(name).map(PathBuf::from);
        let (quota_bytes, mut invalid) = match set("FABER_SCRATCH_QUOTA_BYTES") {
            None => (None, None),
            Some(value) => match value.trim().parse() {
                Ok(bytes) => (Some(bytes), None),
                Err(_) => (
                    None,
                    Some(format!(
                        "FABER_SCRATCH_QUOTA_BYTES must be a whole number of bytes, not `{}`",
                        value.trim()
                    )),
                ),
            },
        };
        let tenantd = match scratch::Endpoint::from_vars("FABER_TENANTD_", &var) {
            Ok(endpoint) => endpoint,
            Err(reason) => {
                invalid.get_or_insert(reason);
                None
            }
        };
        ScratchSettings {
            root: path("FABER_SCRATCH_ROOT"),
            rootfs: path("FABER_SCRATCH_ROOTFS"),
            toolstore: path("FABER_SCRATCH_TOOLSTORE"),
            tenantd,
            image: set("FABER_SCRATCH_IMAGE").map(|image| image.trim().to_owned()),
            profile: set("FABER_SCRATCH_PROFILE").map(|profile| profile.trim().to_owned()),
            quota_bytes,
            invalid,
        }
    }
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
            surge_session_ttl: Duration::from_secs(required_valid(
                "SURGE_SESSION_TTL_SECS",
                72 * 3600,
            )),
            api_port: required_valid("API_PORT", 3001),
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
            allow_local_hosts: required_valid("FABER_ALLOW_LOCAL_HOSTS", true),
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
            preview_connect_timeout: Duration::from_secs(required_valid(
                "FABER_PREVIEW_CONNECT_TIMEOUT_SECS",
                10,
            )),
            preview_header_timeout: Duration::from_secs(required_valid(
                "FABER_PREVIEW_HEADER_TIMEOUT_SECS",
                30,
            )),
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
            search_public_network: required_valid("SEARCH_PUBLIC_NETWORK", false),
            search_proxy: env::var("SEARCH_PROXY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            model_directory_url: env::var("FABER_MODEL_DIRECTORY_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| presets::DEFAULT_URL.to_owned()),
            scratch: ScratchSettings::from_env(),
        }
    }
}

/// `value` parsed, or `default` when it is unset or blank. A value that does
/// not parse is an error rather than the default: a typo must not quietly run
/// the service on settings nobody chose.
fn parse_var<T>(name: &str, value: Option<String>, default: T) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|error| format!("{name} cannot be `{text}`: {error}")),
    }
}

/// The environment variable `name`, stopping startup if its value is invalid.
fn required_valid<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    parse_var(name, env::var(name).ok(), default).unwrap_or_else(|message| panic!("{message}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;

    #[test]
    fn an_unset_or_blank_variable_takes_its_default() {
        assert_eq!(parse_var("API_PORT", None, 3001_u16), Ok(3001));
        assert_eq!(parse_var("API_PORT", Some("  ".into()), 3001_u16), Ok(3001));
    }

    #[test]
    fn a_valid_variable_is_used_and_an_invalid_one_is_refused() {
        assert_eq!(
            parse_var("API_PORT", Some(" 8080 ".into()), 3001_u16),
            Ok(8080)
        );
        let refused = parse_var("API_PORT", Some("30o1".into()), 3001_u16).unwrap_err();
        assert!(
            refused.contains("API_PORT") && refused.contains("30o1"),
            "{refused}"
        );
        assert!(parse_var("SEARCH_PUBLIC_NETWORK", Some("yes".into()), false).is_err());
    }

    fn settings(vars: &[(&str, &str)]) -> ScratchSettings {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        ScratchSettings::from_vars(|name| vars.get(name).cloned())
    }

    #[test]
    fn scratch_settings_come_from_their_variables_and_blank_is_unset() {
        let read = settings(&[
            ("FABER_SCRATCH_ROOT", "/srv/scratch"),
            ("FABER_SCRATCH_ROOTFS", "/srv/rootfs"),
            ("FABER_SCRATCH_TOOLSTORE", "  "),
            ("FABER_TENANTD_SOCKET", "/run/tenantd/api.sock"),
            (
                "FABER_SCRATCH_IMAGE",
                " registry.local/faber/scratch@sha256:abc ",
            ),
            ("FABER_SCRATCH_QUOTA_BYTES", " 1073741824 "),
        ]);
        assert_eq!(read.root.as_deref(), Some(Path::new("/srv/scratch")));
        assert_eq!(read.rootfs.as_deref(), Some(Path::new("/srv/rootfs")));
        assert_eq!(read.toolstore, None);
        assert_eq!(
            read.tenantd,
            Some(scratch::Endpoint::Socket("/run/tenantd/api.sock".into()))
        );
        assert_eq!(
            read.image.as_deref(),
            Some("registry.local/faber/scratch@sha256:abc")
        );
        assert_eq!(read.profile, None);
        assert_eq!(read.quota_bytes, Some(1 << 30));
        assert_eq!(read.invalid, None);

        let none = settings(&[]);
        assert!(none.root.is_none() && none.quota_bytes.is_none() && none.invalid.is_none());
    }

    #[test]
    fn tenantd_over_mtls_needs_all_three_files() {
        let read = settings(&[
            ("FABER_TENANTD_ENDPOINT", "tenantd-1.internal:7443"),
            ("FABER_TENANTD_TLS_CA", "/tls/ca.pem"),
            ("FABER_TENANTD_TLS_CERT", "/tls/faber.pem"),
            ("FABER_TENANTD_TLS_KEY", "/tls/faber.key"),
        ]);
        assert_eq!(
            read.tenantd,
            Some(scratch::Endpoint::Tls(scratch::TlsEndpoint {
                address: "tenantd-1.internal:7443".into(),
                server_name: None,
                ca: "/tls/ca.pem".into(),
                cert: "/tls/faber.pem".into(),
                key: "/tls/faber.key".into(),
            }))
        );
        assert_eq!(read.invalid, None);

        let read = settings(&[
            ("FABER_TENANTD_ENDPOINT", "tenantd-1.internal:7443"),
            ("FABER_TENANTD_TLS_CA", "/tls/ca.pem"),
        ]);
        assert_eq!(read.tenantd, None);
        assert!(read.invalid.unwrap().contains("FABER_TENANTD_TLS_CERT"));

        let read = settings(&[
            ("FABER_TENANTD_SOCKET", "/run/tenantd/api.sock"),
            ("FABER_TENANTD_ENDPOINT", "tenantd-1.internal:7443"),
        ]);
        assert!(read.invalid.is_some());
    }

    #[test]
    fn a_malformed_quota_turns_scratch_off_instead_of_panicking() {
        let read = settings(&[
            ("FABER_SCRATCH_ROOT", "/srv/scratch"),
            ("FABER_SCRATCH_QUOTA_BYTES", "10G"),
        ]);
        assert_eq!(read.quota_bytes, None);
        assert_eq!(
            read.invalid.as_deref(),
            Some("FABER_SCRATCH_QUOTA_BYTES must be a whole number of bytes, not `10G`")
        );
    }
}
