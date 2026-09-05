//! Execution environments — see `internal-docs/host.md`.
//!
//! The host is the registration primitive: every execution mode bottoms out in
//! *reach the machine, then exec*, and only the machine carries authentication
//! and a network path. Containers hang off a host; probes observe one.
//!
//! Nothing here caches liveness. `disabled_at` is operator intent, and
//! `host_probe` is an append-only observation log whose rows are advisory —
//! the authoritative answer to "is it reachable" is the next connection
//! attempt.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::schema::{host, host_container, host_probe, image};

/// How faber reaches the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Local,
    Ssh,
    /// A daemon that dialed out to faber, rather than a host faber dials —
    /// see `internal-docs/agent-transport.md`. Carries no `ssh_address`;
    /// its identity lives in `agent_credential`, keyed by `host_id`.
    Agent,
}

impl Transport {
    pub fn as_str(&self) -> &'static str {
        match self {
            Transport::Local => "local",
            Transport::Ssh => "ssh",
            Transport::Agent => "agent",
        }
    }
}

/// What faber execs into once it has reached the machine.
///
/// Deliberately not derived from `docker_endpoint is not null`: an SSH host that
/// *could* run docker but is deliberately used direct is a real configuration,
/// and collapsing the two loses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecMode {
    Direct,
    Docker,
}

impl ExecMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecMode::Direct => "direct",
            ExecMode::Docker => "docker",
        }
    }
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Host {
    pub id: Uuid,
    /// Who registered this host. Every host is owned: there is no shared
    /// machine faber operates, so every write path filters `user_id = $me`.
    pub user_id: Uuid,
    pub name: String,
    pub transport: String,
    pub exec_mode: String,
    pub ssh_address: Option<String>,
    pub ssh_key_ref: Option<String>,
    /// The SHA256 fingerprint this host is known by. `None` until the first
    /// successful connection, which stores what it saw; every connection after
    /// verifies against it.
    pub ssh_host_key: Option<String>,
    pub docker_endpoint: Option<String>,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
    /// The agent-visible root when this host is used in direct mode. `None`
    /// means the host cannot be bound directly — `/` by default would hand an
    /// agent the whole machine because nobody filled in a field.
    pub root_path: Option<String>,

    /// Docker network selected when resolving a container for live preview.
    /// If absent, only an unambiguous single attached network is accepted.
    pub preview_network: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = host)]
pub struct NewHost<'a> {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: &'a str,
    pub transport: &'a str,
    pub exec_mode: &'a str,
    pub ssh_address: Option<&'a str>,
    pub ssh_key_ref: Option<&'a str>,
    pub ssh_host_key: Option<&'a str>,
    pub docker_endpoint: Option<&'a str>,
    pub root_path: Option<&'a str>,
    pub preview_network: Option<&'a str>,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = host)]
pub struct UpdateHost<'a> {
    pub name: Option<&'a str>,
    pub transport: Option<&'a str>,
    pub exec_mode: Option<&'a str>,
    pub ssh_address: Option<Option<&'a str>>,
    pub ssh_key_ref: Option<Option<&'a str>>,
    /// `Some(None)` clears it, which is how a rebuilt machine is re-trusted:
    /// deliberately, by the operator, rather than automatically on mismatch.
    pub ssh_host_key: Option<Option<&'a str>>,
    pub docker_endpoint: Option<Option<&'a str>>,
    /// Operator intent. `Some(None)` re-enables; the column never reflects an
    /// observation.
    pub disabled_at: Option<Option<DateTime<Utc>>>,
    pub root_path: Option<Option<&'a str>>,
    pub preview_network: Option<Option<&'a str>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host_container)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HostContainer {
    pub id: Uuid,
    pub host_id: Uuid,
    /// Who this container belongs to, recorded here so authorization reads
    /// the container row rather than joining through its host.
    pub user_id: Uuid,
    pub container_ref: String,
    pub name: Option<String>,
    pub root_path: String,
    pub created_at: DateTime<Utc>,
    pub unregistered_at: Option<DateTime<Utc>>,
    /// When faber created this container. `None` means the user did and faber
    /// was only told about it — which is the difference between a container
    /// faber may destroy and one it must leave alone.
    pub managed_at: Option<DateTime<Utc>>,
    /// The template it was created from, kept as provenance. Nothing resolves
    /// through it, and it goes null if the template is deleted.
    pub image_id: Option<Uuid>,
}

impl HostContainer {
    /// Faber created it, so faber may destroy it.
    pub fn managed(&self) -> bool {
        self.managed_at.is_some()
    }
}

#[derive(Insertable)]
#[diesel(table_name = host_container)]
pub struct NewHostContainer<'a> {
    pub id: Uuid,
    pub host_id: Uuid,
    pub user_id: Uuid,
    pub container_ref: &'a str,
    pub name: Option<&'a str>,
    pub root_path: &'a str,
    pub managed_at: Option<DateTime<Utc>>,
    pub image_id: Option<Uuid>,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = host_container)]
pub struct UpdateHostContainer<'a> {
    pub container_ref: Option<&'a str>,
    pub name: Option<Option<&'a str>>,
    pub root_path: Option<&'a str>,
    /// State of the *registration*, not of the container. A container the user
    /// removed out of band stays registered until someone says otherwise.
    pub unregistered_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host_probe)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HostProbe {
    pub id: Uuid,
    pub host_id: Uuid,
    pub container_id: Option<Uuid>,
    pub probed_at: DateTime<Utc>,
    pub ok: bool,
    pub error: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub shell: Option<String>,
    pub tools: Option<Value>,
    pub root_path: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = host_probe)]
pub struct NewHostProbe<'a> {
    pub id: Uuid,
    pub host_id: Uuid,
    pub container_id: Option<Uuid>,
    pub ok: bool,
    pub error: Option<&'a str>,
    pub os: Option<&'a str>,
    pub arch: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub tools: Option<Value>,
    pub root_path: Option<&'a str>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = image)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Image {
    pub id: Uuid,
    /// Who registered this template. Every image is owned, like every host.
    /// Ownership is enforced by SQL filters on the way in; the loaded value
    /// itself is never inspected.
    #[allow(dead_code)]
    pub user_id: Uuid,
    pub name: String,
    pub reference: String,
    pub default_mounts: Option<Value>,
    pub default_root_path: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = image)]
pub struct NewImage<'a> {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: &'a str,
    pub reference: &'a str,
    pub default_mounts: Option<Value>,
    pub default_root_path: &'a str,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = image)]
pub struct UpdateImage<'a> {
    pub name: Option<&'a str>,
    pub reference: Option<&'a str>,
    pub default_mounts: Option<Option<Value>>,
    pub default_root_path: Option<&'a str>,
}


