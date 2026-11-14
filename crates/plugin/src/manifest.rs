//! The manifest: static data about a plugin type, read without instantiating
//! it.
//!
//! For a component it is UTF-8 JSON in the `faber:manifest` custom section;
//! for a built-in, a value of the same shape. Either way the core refuses one
//! that is missing or fails [`Manifest::check`], and everything here is data:
//! exports exist only for what computes.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema;

/// The custom section a component carries its manifest in.
pub const SECTION: &str = "faber:manifest";

/// Tool names, prefix included, are this long at most.
pub const MAX_TOOL_NAME: usize = 64;

/// What joins a plugin id to a tool name. Ids may not contain it, so the first
/// occurrence in a sent name is always the join.
pub const SEPARATOR: &str = "__";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    /// The plugin's own semver, separate from the contract and config
    /// versions.
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    pub config: ConfigSpec,
}

/// What the host grants. Each key is present only when requested, and each
/// applies to the plugin's daemon and its tool handlers alike.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpCapability>,
    /// One long-lived daemon serving every enabled binding of this version.
    /// The plugin must implement one, and a plugin that implements one must
    /// request this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonCapability>,
    /// Outbound `wss` connections. Only a daemon holds sockets, so this needs
    /// `daemon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub websocket: Option<WebsocketCapability>,
    /// The binding's private key-value store, for the daemon and the tool
    /// handlers both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageCapability>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HttpCapability {
    /// Host names. A leading `*.` matches any subdomain: `*.example.com`
    /// matches `api.example.com`, not `example.com`.
    pub hosts: Vec<String>,
}

impl HttpCapability {
    /// Whether a request to `host` is allowed. Redirects are checked the same
    /// way, hop by hop.
    pub fn allows(&self, host: &str) -> bool {
        host_allowed(&self.hosts, host)
    }
}

/// `"daemon": {}`. No settings yet; an object so it can grow some.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonCapability {}

/// `"storage": {}`. No settings yet; the caps are the operator's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCapability {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebsocketCapability {
    /// Host names, matched exactly as [`HttpCapability::hosts`] are.
    pub hosts: Vec<String>,
}

impl WebsocketCapability {
    /// Whether a connection to `host` is allowed.
    pub fn allows(&self, host: &str) -> bool {
        host_allowed(&self.hosts, host)
    }

    /// Whether a connection to `url` is allowed: `wss` only, to a listed host.
    pub fn allows_url(&self, url: &str) -> Result<(), String> {
        let host = wss_host(url)?;
        if self.allows(&host) {
            Ok(())
        } else {
            Err(format!("`{host}` is not in the manifest's websocket hosts"))
        }
    }
}

/// The host a `wss://` URL connects to, lowercased and without its port.
/// Every other scheme is refused, `ws` included, and so is a URL carrying
/// credentials.
pub fn wss_host(url: &str) -> Result<String, String> {
    let rest = url
        .split_once("://")
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("wss"))
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("`{url}` is not a wss:// URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(format!("`{url}` carries credentials"));
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    if host.is_empty() || host.starts_with('[') {
        return Err(format!("`{url}` names no host name"));
    }
    Ok(host.to_ascii_lowercase())
}

/// The one host-matching rule every capability with `hosts` uses.
fn host_allowed(hosts: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    hosts.iter().any(|pattern| {
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|head| head.len() > 1 && head.ends_with('.')),
            None => host == pattern,
        }
    })
}

/// Host patterns that are not host names, each as a [`ManifestError`].
fn check_hosts(field: &str, hosts: &[String], found: &mut Vec<ManifestError>) {
    for host in hosts {
        let bare = host.strip_prefix("*.").unwrap_or(host);
        if bare.is_empty() || bare.contains('*') || bare.contains('/') || bare.contains(':') {
            found.push(error(field, format!("`{host}` is not a host name")));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// The declared name, unique within the plugin. The model sees it
    /// prefixed: `<plugin id>__<name>`.
    pub name: String,
    pub description: String,
    /// JSON Schema; the core checks every call against it.
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigSpec {
    /// A positive integer, separate from the contract version.
    pub version: u32,
    pub schema: Value,
    pub default: Value,
}

/// One rule a manifest broke.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ManifestError {
    pub field: String,
    pub message: String,
}

impl Manifest {
    /// Reads a manifest out of its custom-section bytes.
    pub fn from_section(bytes: &[u8]) -> Result<Manifest, Vec<ManifestError>> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| vec![error("manifest", "is not UTF-8")])?;
        let manifest: Manifest = serde_json::from_str(text)
            .map_err(|parse| vec![error("manifest", format!("is not a manifest: {parse}"))])?;
        manifest.check()?;
        Ok(manifest)
    }

    /// Every rule the manifest format sets that can be checked without the
    /// plugin. That `default` passes the plugin's own `validate` is checked by
    /// the registry, which has the plugin.
    pub fn check(&self) -> Result<(), Vec<ManifestError>> {
        let mut found = Vec::new();

        if let Err(message) = check_id(&self.id) {
            found.push(error("id", message));
        }
        if self.name.trim().is_empty() {
            found.push(error("name", "is empty"));
        }
        if !is_semver(&self.version) {
            found.push(error(
                "version",
                format!("`{}` is not a semver", self.version),
            ));
        }

        let capabilities = &self.capabilities;
        if let Some(http) = &capabilities.http {
            check_hosts("capabilities.http.hosts", &http.hosts, &mut found);
        }
        if let Some(websocket) = &capabilities.websocket {
            check_hosts("capabilities.websocket.hosts", &websocket.hosts, &mut found);
            if capabilities.daemon.is_none() {
                found.push(error(
                    "capabilities.websocket",
                    "needs `daemon`: only a daemon holds sockets",
                ));
            }
        }

        let mut names = BTreeSet::new();
        for (index, tool) in self.tools.iter().enumerate() {
            let field = format!("tools[{index}].name");
            if !names.insert(tool.name.as_str()) {
                found.push(error(&field, format!("`{}` is declared twice", tool.name)));
            }
            if let Err(message) = check_tool_name(&tool.name) {
                found.push(error(&field, message));
            }
            let sent = tool_name(&self.id, &tool.name);
            if sent.len() > MAX_TOOL_NAME {
                found.push(error(
                    &field,
                    format!("`{sent}` is longer than {MAX_TOOL_NAME} characters"),
                ));
            }
            if !tool.input_schema.is_object() {
                found.push(error(
                    format!("tools[{index}].input_schema"),
                    "is not a JSON Schema object",
                ));
            }
        }

        if self.config.version == 0 {
            found.push(error("config.version", "must be a positive integer"));
        }
        if !schema::is_schema(&self.config.schema) {
            found.push(error("config.schema", "is not a JSON Schema"));
        } else {
            for violation in schema::check(&self.config.schema, &self.config.default) {
                found.push(error(
                    "config.default",
                    format!("does not pass the config schema: {violation}"),
                ));
            }
        }

        if found.is_empty() { Ok(()) } else { Err(found) }
    }

    pub fn tool(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.iter().find(|tool| tool.name == name)
    }
}

/// The name a tool is sent to the model as.
pub fn tool_name(plugin: &str, tool: &str) -> String {
    format!("{plugin}{SEPARATOR}{tool}")
}

/// Splits a sent tool name back into plugin id and declared name.
pub fn split_tool_name(sent: &str) -> Option<(&str, &str)> {
    sent.split_once(SEPARATOR)
}

/// Plugin ids: a lowercase letter, then lowercase letters, digits, `_` and
/// `-`; never `__`, and never a trailing `_` (which would put a third `_`
/// against the separator and make the join ambiguous). Short enough to leave
/// room for a tool name inside [`MAX_TOOL_NAME`].
pub fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 32 {
        return Err("must be 1 to 32 characters".into());
    }
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!("`{id}` must start with a lowercase letter"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "`{id}` may contain only lowercase letters, digits, `_` and `-`"
        ));
    }
    if id.contains(SEPARATOR) {
        return Err(format!("`{id}` may not contain `{SEPARATOR}`"));
    }
    if id.ends_with('_') {
        return Err(format!("`{id}` may not end with `_`"));
    }
    Ok(())
}

/// Tool names: what every wire accepts in a tool name, and not starting with
/// `_`, for the same reason an id does not end with one.
fn check_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("is empty".into());
    }
    if name.starts_with('_') {
        return Err(format!("`{name}` may not start with `_`"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "`{name}` may contain only letters, digits, `_` and `-`"
        ));
    }
    Ok(())
}

/// `MAJOR.MINOR.PATCH`, with an optional `-pre` and `+build`.
pub fn is_semver(version: &str) -> bool {
    let core = version.split_once('+').map_or(version, |(core, _)| core);
    let core = core.split_once('-').map_or(core, |(core, _)| core);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.chars().all(|c| c.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

fn error(field: impl Into<String>, message: impl Into<String>) -> ManifestError {
    ManifestError {
        field: field.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn weather() -> Manifest {
        serde_json::from_value(json!({
            "id": "weather",
            "name": "Weather",
            "version": "1.2.0",
            "description": "Forecasts for a place.",
            "capabilities": { "http": { "hosts": ["api.open-meteo.com", "*.example.com"] } },
            "tools": [{
                "name": "forecast",
                "description": "Get the forecast for a place.",
                "input_schema": { "type": "object" }
            }],
            "config": { "version": 2, "schema": { "type": "object" }, "default": {} }
        }))
        .unwrap()
    }

    #[test]
    fn the_spec_example_is_a_valid_manifest() {
        weather().check().unwrap();
        let bytes = serde_json::to_vec(&weather()).unwrap();
        assert_eq!(Manifest::from_section(&bytes).unwrap(), weather());
    }

    #[test]
    fn ids_cannot_make_a_tool_name_ambiguous() {
        assert!(check_id("github").is_ok());
        assert!(check_id("my-plugin_2").is_ok());
        assert!(check_id("a__b").is_err());
        assert!(check_id("trailing_").is_err());
        assert!(check_id("Upper").is_err());
        assert!(check_id("9lives").is_err());
        assert_eq!(tool_name("github", "open_pr"), "github__open_pr");
        assert_eq!(
            split_tool_name("github__open_pr"),
            Some(("github", "open_pr"))
        );
        assert_eq!(split_tool_name("a_b__c__d"), Some(("a_b", "c__d")));
    }

    #[test]
    fn broken_rules_are_each_reported() {
        let mut manifest = weather();
        manifest.version = "1.2".into();
        manifest.tools.push(manifest.tools[0].clone());
        manifest.tools.push(ToolDefinition {
            name: "x".repeat(60),
            description: String::new(),
            input_schema: json!({}),
        });
        manifest.config.version = 0;
        manifest.config.default = json!([]);
        let errors = manifest.check().unwrap_err();
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"version"));
        assert!(fields.contains(&"tools[1].name"));
        assert!(fields.contains(&"tools[2].name"));
        assert!(fields.contains(&"config.version"));
        assert!(fields.contains(&"config.default"));
    }

    #[test]
    fn a_wildcard_host_matches_subdomains_only() {
        let http = weather().capabilities.http.unwrap();
        assert!(http.allows("api.open-meteo.com"));
        assert!(http.allows("api.example.com"));
        assert!(http.allows("a.b.example.com"));
        assert!(!http.allows("example.com"));
        assert!(!http.allows("evilexample.com"));
        assert!(!http.allows("open-meteo.com"));
    }

    fn discord() -> Manifest {
        let mut manifest = weather();
        manifest.capabilities = serde_json::from_value(json!({
            "http": { "hosts": ["discord.com"] },
            "daemon": {},
            "websocket": { "hosts": ["gateway.discord.gg", "*.discord.gg"] },
            "storage": {}
        }))
        .unwrap();
        manifest
    }

    #[test]
    fn the_daemon_capabilities_parse_and_round_trip() {
        let manifest = discord();
        manifest.check().unwrap();
        assert!(manifest.capabilities.daemon.is_some());
        assert!(manifest.capabilities.storage.is_some());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        assert_eq!(Manifest::from_section(&bytes).unwrap(), manifest);
        // A v1 manifest serializes without the new keys.
        let v1 = serde_json::to_value(weather()).unwrap();
        assert_eq!(v1["capabilities"], json!({ "http": weather_hosts() }));
    }

    fn weather_hosts() -> serde_json::Value {
        json!({ "hosts": ["api.open-meteo.com", "*.example.com"] })
    }

    #[test]
    fn websocket_without_daemon_is_refused() {
        let mut manifest = discord();
        manifest.capabilities.daemon = None;
        let errors = manifest.check().unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "capabilities.websocket");

        let mut manifest = discord();
        manifest.capabilities.websocket.as_mut().unwrap().hosts = vec!["wss://x.gg".into()];
        let errors = manifest.check().unwrap_err();
        assert_eq!(errors[0].field, "capabilities.websocket.hosts");
    }

    #[test]
    fn websocket_hosts_match_like_http_and_only_over_wss() {
        let websocket = discord().capabilities.websocket.unwrap();
        let http = HttpCapability {
            hosts: websocket.hosts.clone(),
        };
        for host in [
            "gateway.discord.gg",
            "us-east.discord.gg",
            "discord.gg",
            "evildiscord.gg",
        ] {
            assert_eq!(websocket.allows(host), http.allows(host), "{host}");
        }
        assert!(!websocket.allows("discord.gg"));

        assert_eq!(
            websocket.allows_url("wss://gateway.discord.gg/?v=10&encoding=json"),
            Ok(())
        );
        assert_eq!(websocket.allows_url("WSS://Gateway.Discord.gg:443"), Ok(()));
        assert!(websocket.allows_url("ws://gateway.discord.gg").is_err());
        assert!(websocket.allows_url("https://gateway.discord.gg").is_err());
        assert!(
            websocket
                .allows_url("wss://evil.com/gateway.discord.gg")
                .is_err()
        );
        assert!(
            websocket
                .allows_url("wss://u:p@gateway.discord.gg")
                .is_err()
        );
        assert!(websocket.allows_url("gateway.discord.gg").is_err());
    }

    #[test]
    fn semver_needs_three_numeric_parts() {
        assert!(is_semver("1.0.0"));
        assert!(is_semver("1.0.0-rc.1+build.5"));
        assert!(!is_semver("1.0"));
        assert!(!is_semver("01.0.0"));
        assert!(!is_semver("1.x.0"));
    }
}
