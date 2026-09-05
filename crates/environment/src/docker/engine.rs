//! The Engine API calls this crate needs, and nothing else.
//!
//! Pinned to `v1.41` — old enough that any daemon a user is plausibly running
//! speaks it, new enough to carry everything here. An unversioned path would
//! follow whatever the daemon defaults to, which makes Faber's behavior a
//! function of someone else's upgrade schedule.
//!
//! Every call opens its own connection. See [`Daemon`] for why.
//!
//! The exec and archive calls are what a bound [`Target`](crate::Target) runs
//! on. [`container_create`], [`container_start`], [`container_remove`], and
//! [`image_pull`] are not: nothing on the `Target` surface reaches them, and
//! nothing may be added that does. They exist for the consumer that *creates*
//! a container on a user's instruction — the same asymmetry the whole crate
//! carries, where the user decides what exists and the agent only works
//! inside it.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::docker::daemon::{Conn, Daemon};
use crate::docker::http::Wire;
use crate::fault::{Denial, Fault};

/// The API version every path is prefixed with.
const API: &str = "/v1.41";

/// What an exec is doing, as the daemon sees it.
pub struct ExecState {
    pub running: bool,
    pub exit_code: Option<i64>,
}

/// Network facts needed to address a service inside a container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerNetworks {
    /// Docker's network mode (`default`, `bridge`, `host`, `none`, or a named
    /// network). The caller owns policy for which modes it will publish.
    pub mode: String,
    /// Attached network name and non-empty IP address pairs.
    pub addresses: Vec<(String, String)>,
}

/// Reads only the network portion of one container's inspect response.
pub async fn container_networks(
    daemon: &Arc<dyn Daemon>,
    container: &str,
) -> Result<ContainerNetworks, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "GET",
            &format!("{API}/containers/{}/json", encode(container)),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;
    if !head.ok() {
        return Err(refused(head.status, &payload, container));
    }

    parse_container_networks(&payload)
}

fn parse_container_networks(payload: &[u8]) -> Result<ContainerNetworks, Fault> {
    let value: Value = serde_json::from_slice(payload)
        .map_err(|_| Fault::Unreachable("the daemon sent unreadable container state".into()))?;
    let mode = value
        .pointer("/HostConfig/NetworkMode")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut addresses = value
        .pointer("/NetworkSettings/Networks")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|networks| networks.iter())
        .filter_map(|(name, network)| {
            let address = network.get("IPAddress")?.as_str()?.trim();
            (!address.is_empty()).then(|| (name.clone(), address.to_owned()))
        })
        .collect::<Vec<_>>();
    addresses.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(ContainerNetworks { mode, addresses })
}

/// Creates an exec and returns its id. Does not start it.
pub async fn exec_create(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    cmd: &[String],
    env: &[(String, String)],
    workdir: &str,
    tty: bool,
) -> Result<String, Fault> {
    let body = json!({
        "AttachStdin": true,
        "AttachStdout": true,
        "AttachStderr": true,
        "Tty": tty,
        "Cmd": cmd,
        "Env": env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "WorkingDir": workdir,
    });

    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "POST",
            &format!("{API}/containers/{}/exec", encode(container)),
            Some(body.to_string().as_bytes()),
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    if !head.ok() {
        return Err(refused(head.status, &payload, container));
    }

    serde_json::from_slice::<Value>(&payload)
        .ok()
        .and_then(|value| value.get("Id")?.as_str().map(str::to_owned))
        .ok_or_else(|| Fault::Unreachable("the daemon created an exec with no id".to_owned()))
}

/// Starts an exec and hijacks the connection.
///
/// What comes back is the raw bidirectional stream: stdin goes up it
/// unframed, and stdout and stderr come down it multiplexed, which is why
/// [`super::spawn`] has to demultiplex.
pub async fn exec_start(daemon: &Arc<dyn Daemon>, exec: &str, tty: bool) -> Result<Conn, Fault> {
    let body = json!({ "Detach": false, "Tty": tty });

    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "POST",
            &format!("{API}/exec/{}/start", encode(exec)),
            Some(body.to_string().as_bytes()),
            true,
        )
        .await?;

    // 101 is the hijack. Some daemons answer 200 and stream instead, which
    // works identically for reading and simply will not carry stdin.
    if head.status != 101 && head.status != 200 {
        let payload = wire.body(&head).await?;
        return Err(refused(head.status, &payload, exec));
    }
    Ok(wire.hijack())
}

pub async fn exec_inspect(daemon: &Arc<dyn Daemon>, exec: &str) -> Result<ExecState, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "GET",
            &format!("{API}/exec/{}/json", encode(exec)),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    if !head.ok() {
        return Err(refused(head.status, &payload, exec));
    }

    let value: Value = serde_json::from_slice(&payload)
        .map_err(|_| Fault::Unreachable("the daemon sent an unreadable exec state".to_owned()))?;
    Ok(ExecState {
        running: value
            .get("Running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        exit_code: value.get("ExitCode").and_then(Value::as_i64),
    })
}

/// Pulls a path out of the container as a tar stream.
pub async fn archive_get(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    path: &str,
) -> Result<Vec<u8>, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "GET",
            &format!(
                "{API}/containers/{}/archive?path={}",
                encode(container),
                encode(path)
            ),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    match head.status {
        200 => Ok(payload),
        404 => Err(Fault::Denied(Denial::NotFound {
            path: path.to_owned(),
        })),
        status => Err(refused(status, &payload, path)),
    }
}

/// Unpacks a tar into a directory that already exists in the container.
pub async fn archive_put(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    directory: &str,
    tar: &[u8],
) -> Result<(), Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request_tar(
            &format!(
                "{API}/containers/{}/archive?path={}",
                encode(container),
                encode(directory)
            ),
            tar,
        )
        .await?;
    let payload = wire.body(&head).await?;

    match head.status {
        200 => Ok(()),
        404 => Err(Fault::Denied(Denial::NotFound {
            path: directory.to_owned(),
        })),
        status => Err(refused(status, &payload, directory)),
    }
}

/// What the daemon knows about one path, without moving its contents.
///
/// The stat rides in a base64 header rather than the body, which is why this
/// is a `HEAD` and why it is worth having: it answers "does this exist, and is
/// it a directory" in one round trip and without pulling a byte of content.
pub async fn archive_stat(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    path: &str,
) -> Result<Option<Value>, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "HEAD",
            &format!(
                "{API}/containers/{}/archive?path={}",
                encode(container),
                encode(path)
            ),
            None,
            false,
        )
        .await?;

    match head.status {
        200 => {
            let Some(encoded) = head.header("x-docker-container-path-stat") else {
                return Ok(None);
            };
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| Fault::Unreachable("unreadable path stat".to_owned()))?;
            Ok(serde_json::from_slice(&raw).ok())
        }
        404 => Ok(None),
        status => Err(refused(status, &[], path)),
    }
}

// ---------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------

/// One bind mount, as the consumer described it.
///
/// Only bind mounts. A named volume is a thing with a lifetime of its own that
/// outlives the container and that nobody here would ever delete, and offering
/// one would quietly make Faber the owner of storage it cannot account for.
pub struct Mount {
    /// The path on the machine the daemon runs on — not on the Faber host.
    pub source: String,
    /// Where it appears inside the container.
    pub target: String,
    pub read_only: bool,
}

/// Everything the daemon needs to create a container.
pub struct Create<'a> {
    /// The daemon assigns a random name when this is `None`. Passing one makes
    /// the container findable by a human on the machine it lives on, which is
    /// the difference between a container a user can reason about and one they
    /// find later and dare not delete.
    pub name: Option<&'a str>,
    pub image: &'a str,
    /// The process the container runs. It has to be one that stays up: a
    /// container exists here to be exec'd into, and one whose entrypoint
    /// returned is a stopped container with a confusing history.
    pub cmd: &'a [String],
    pub env: &'a [(String, String)],
    pub working_dir: &'a str,
    pub mounts: &'a [Mount],
    /// Written onto the container so the machine's owner can tell what created
    /// it without consulting Faber's database.
    pub labels: &'a [(String, String)],
}

/// Creates a container and returns its id. Does not start it.
///
/// A missing image is reported as [`Denial::NotFound`] naming the reference,
/// so a caller that wants to pull and retry can tell that case apart from a
/// daemon that refused for some other reason.
pub async fn container_create(
    daemon: &Arc<dyn Daemon>,
    create: &Create<'_>,
) -> Result<String, Fault> {
    let body = json!({
        "Image": create.image,
        "Cmd": create.cmd,
        "Env": create.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "WorkingDir": create.working_dir,
        "Labels": create.labels.iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<serde_json::Map<_, _>>(),
        "HostConfig": {
            "Mounts": create.mounts.iter().map(|mount| json!({
                "Type": "bind",
                "Source": mount.source,
                "Target": mount.target,
                "ReadOnly": mount.read_only,
            })).collect::<Vec<_>>(),
        },
    });

    let target = match create.name {
        Some(name) => format!("{API}/containers/create?name={}", encode(name)),
        None => format!("{API}/containers/create"),
    };

    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request("POST", &target, Some(body.to_string().as_bytes()), false)
        .await?;
    let payload = wire.body(&head).await?;

    if head.status == 404 {
        return Err(Fault::Denied(Denial::NotFound {
            path: create.image.to_owned(),
        }));
    }
    if !head.ok() {
        return Err(refused(head.status, &payload, create.image));
    }

    serde_json::from_slice::<Value>(&payload)
        .ok()
        .and_then(|value| value.get("Id")?.as_str().map(str::to_owned))
        .ok_or_else(|| Fault::Unreachable("the daemon created a container with no id".to_owned()))
}

/// Starts a created container. Starting one already running is not an error —
/// the daemon answers 304, and the caller asked for a running container, which
/// is what it has.
pub async fn container_start(daemon: &Arc<dyn Daemon>, container: &str) -> Result<(), Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "POST",
            &format!("{API}/containers/{}/start", encode(container)),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    match head.status {
        200 | 204 | 304 => Ok(()),
        status => Err(refused(status, &payload, container)),
    }
}

/// Removes a container, killing it first if it is running.
///
/// Reachable only from the consumer side, and only for a container Faber
/// created: destroying one a user made themselves is destroying something
/// Faber was merely told about.
pub async fn container_remove(daemon: &Arc<dyn Daemon>, container: &str) -> Result<(), Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "DELETE",
            &format!("{API}/containers/{}?force=true", encode(container)),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    match head.status {
        // Already gone is the state the caller asked for.
        200 | 204 | 404 => Ok(()),
        status => Err(refused(status, &payload, container)),
    }
}

/// Pulls an image, waiting for the pull to finish.
///
/// The daemon answers 200 immediately and then streams progress as JSON lines,
/// reporting a failure *inside* that stream rather than in the status — so a
/// pull that could not authenticate looks exactly like a successful one until
/// the body is read to the end and inspected.
pub async fn image_pull(daemon: &Arc<dyn Daemon>, reference: &str) -> Result<(), Fault> {
    let (image, tag) = split_tag(reference);
    let target = format!(
        "{API}/images/create?fromImage={}&tag={}",
        encode(image),
        encode(tag)
    );

    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire.request("POST", &target, None, false).await?;
    let payload = wire.body(&head).await?;

    if !head.ok() {
        return Err(refused(head.status, &payload, reference));
    }

    for line in payload.split(|byte| *byte == b'\n') {
        if let Ok(value) = serde_json::from_slice::<Value>(line)
            && let Some(error) = value.get("error").and_then(Value::as_str)
        {
            return Err(Fault::Denied(Denial::Malformed {
                what: "image reference".into(),
                reason: format!("could not pull `{reference}`: {error}"),
            }));
        }
    }
    Ok(())
}

/// Splits `repo/name:tag` into its two halves, defaulting to `latest`.
///
/// Two things this has to get right. The colon is searched for after the last
/// `/`, or a registry carrying a port — `registry.local:5000/app` — loses its
/// port to the tag. And a digest reference is split on its `@`, since the
/// daemon takes `sha256:…` whole as the tag.
fn split_tag(reference: &str) -> (&str, &str) {
    if let Some(at) = reference.rfind('@') {
        return (&reference[..at], &reference[at + 1..]);
    }
    let start = reference.rfind('/').map_or(0, |at| at + 1);
    match reference[start..].rfind(':') {
        Some(offset) => {
            let at = start + offset;
            (&reference[..at], &reference[at + 1..])
        }
        None => (reference, "latest"),
    }
}

/// A daemon's refusal, carried across as the class the agent should act on.
///
/// A 4xx is about the request and a 5xx is about the daemon, which is the same
/// split the agent already knows how to repair.
fn refused(status: u16, payload: &[u8], what: &str) -> Fault {
    let message = serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("the daemon answered {status}"));

    match status {
        404 => Fault::Denied(Denial::NotFound {
            path: what.to_owned(),
        }),
        // A stopped container cannot exec. It is not gone and the request was
        // not malformed, so the honest answer is that it could not be reached.
        409 => Fault::Unreachable(format!("`{what}` is not running: {message}")),
        400..=499 => Fault::Denied(Denial::Malformed {
            what: "docker request".into(),
            reason: message,
        }),
        _ => Fault::Unreachable(message),
    }
}

/// Percent-encodes a query value. Container refs and paths both reach here,
/// and a path with a space in it is not a malformed request.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_a_space_survives_the_query_string() {
        assert_eq!(encode("/a b/c"), "%2Fa%20b%2Fc");
        assert_eq!(encode("plain-name_1.2~"), "plain-name_1.2~");
    }

    #[test]
    fn a_stopped_container_is_unreachable_rather_than_a_denial() {
        let fault = refused(409, br#"{"message":"is not running"}"#, "build");
        assert!(matches!(fault, Fault::Unreachable(_)));
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        assert_eq!(split_tag("alpine"), ("alpine", "latest"));
        assert_eq!(split_tag("alpine:3.20"), ("alpine", "3.20"));
        assert_eq!(
            split_tag("registry.local:5000/app"),
            ("registry.local:5000/app", "latest")
        );
        assert_eq!(
            split_tag("registry.local:5000/app:v1"),
            ("registry.local:5000/app", "v1")
        );
        assert_eq!(split_tag("alpine@sha256:abc"), ("alpine", "sha256:abc"));
    }

    #[test]
    fn a_missing_path_is_not_found_whatever_the_body_says() {
        let fault = refused(404, b"", "/nope");
        assert!(matches!(fault, Fault::Denied(Denial::NotFound { .. })));
    }

    #[test]
    fn container_network_inspection_keeps_named_nonempty_addresses() {
        let networks = parse_container_networks(
            br#"{
                "HostConfig":{"NetworkMode":"bridge"},
                "NetworkSettings":{"Networks":{
                    "z":{"IPAddress":"10.2.0.3"},
                    "empty":{"IPAddress":""},
                    "a":{"IPAddress":"10.1.0.3"}
                }}
            }"#,
        )
        .unwrap();
        assert_eq!(networks.mode, "bridge");
        assert_eq!(
            networks.addresses,
            vec![
                ("a".into(), "10.1.0.3".into()),
                ("z".into(), "10.2.0.3".into())
            ]
        );
    }
}
