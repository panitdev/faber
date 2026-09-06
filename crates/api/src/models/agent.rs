//! Agent-transport credentials — see `internal-docs/agent-transport.md`.
//!
//! Two tables, two different secrets. `agent_enrollment` holds the one-time
//! bootstrap token exchanged at first daemon run; `agent_credential` holds
//! the long-lived token every reconnect presents, plus the SSH host key
//! pinned at that same exchange. Neither `token_hash` is ever compared in
//! plaintext — both are looked up by hash and verified with a constant-time
//! comparison at the call site, the same shape as any bearer credential.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::{agent_credential, agent_enrollment};

#[derive(Insertable)]
#[diesel(table_name = agent_enrollment)]
pub struct NewAgentEnrollment<'a> {
    pub id: Uuid,
    pub host_id: Uuid,
    pub token_hash: &'a str,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = agent_credential)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AgentCredential {
    pub id: Uuid,
    pub host_id: Uuid,
    pub token_hash: String,
    /// The daemon's SSH host public key, reported at enrollment — not the
    /// SHA256 fingerprint `host.ssh_host_key` holds for an SSH host.
    /// `HostKey::Verify` wants the fingerprint, so the conversion happens at
    /// bind (`environment::ssh::fingerprint_of`).
    pub host_pubkey: String,
    pub issued_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Insertable)]
#[diesel(table_name = agent_credential)]
pub struct NewAgentCredential<'a> {
    pub id: Uuid,
    pub host_id: Uuid,
    pub token_hash: &'a str,
    pub host_pubkey: &'a str,
}
