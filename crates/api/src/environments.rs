//! Reaching a user's machine from a request handler.
//!
//! Everything the `environment` crate does starts from something dialled, and
//! what to dial is always configuration passed in — never the process's own.
//! This module is the one place that configuration is assembled, and it
//! assembles it from the caller's own rows: `host.docker_endpoint`,
//! `host.ssh_address`, and the credential `host.ssh_key_ref` names. Faber runs
//! as a multi-user service, so `DOCKER_HOST`, `~/.ssh`, an agent socket, and a
//! docker context describe the operator and would send one user's work out
//! under another's identity. None of them is read here, and none should be.
//!
//! Daemon connections remain one-call streams. SSH sessions are shared by the
//! process-wide pool because commands, Docker forwarding, and live previews
//! are all channels on the same authenticated transport. The pool identity
//! includes the credential id, address, account, and pinned host key, so a key
//! rotation or host reconfiguration cannot silently reuse the old session.

use std::sync::Arc;

use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use environment::docker::{Daemon, LocalSocket};
use environment::ssh::forward::DOCKER_SOCKET;
use environment::ssh::{SshForwarded, SshSession};
use environment::{Blobs, DockerTarget, LocalTarget, Machine, Registry, Root, SshTarget};
use environment::{Denial, Fault};
use uuid::Uuid;

use crate::{
    error::{ApiResult, AppError},
    models::{
        host::{ExecMode, Host, HostContainer, Transport},
        now_epoch,
        session::{NewSessionEnvironment, SessionEnvironment},
    },
    schema::{host, host_container, session_environment},
    state::AppState,
};

/// Opens a path to the container daemon a host runs.
///
/// Refuses a host that is not in docker mode: `exec_mode` is the user's
/// statement of what faber does once it has reached the machine, and treating
/// a `direct` host as a docker one because it happens to have an endpoint set
/// would override that statement with a guess.
pub async fn reach_daemon(
    state: &AppState,
    user_id: Uuid,
    host: &Host,
) -> ApiResult<Arc<dyn Daemon>> {
    if host.exec_mode != ExecMode::Docker.as_str() {
        return Err(AppError::BadRequest(
            "this host's exec_mode is not 'docker', so it runs no container daemon".into(),
        ));
    }
    if host.disabled_at.is_some() {
        return Err(AppError::BadRequest("this host is disabled".into()));
    }

    if host.transport == Transport::Ssh.as_str() {
        let session = state.ssh.get(state, user_id, host).await?;

        // The socket path is on the *far* side, so a `tcp://` endpoint is not
        // a thing this forward can dial: `direct-streamlocal` opens a unix
        // socket, and asking a user to expose their docker socket to the
        // network instead is a bad trade made on their behalf.
        let socket = match host.docker_endpoint.as_deref() {
            None => DOCKER_SOCKET.to_owned(),
            Some(endpoint) if endpoint.starts_with("unix://") => {
                endpoint.trim_start_matches("unix://").to_owned()
            }
            Some(endpoint) if endpoint.starts_with('/') => endpoint.to_owned(),
            Some(endpoint) => {
                return Err(AppError::BadRequest(format!(
                    "`{endpoint}` cannot be reached over ssh; \
                     name the daemon's socket path on the far side"
                )));
            }
        };

        return Ok(Arc::new(SshForwarded::new(session, socket)));
    }

    if host.transport == Transport::Agent.as_str() {
        let session = agent_session(state, host)?;

        let socket = match host.docker_endpoint.as_deref() {
            None => DOCKER_SOCKET.to_owned(),
            Some(endpoint) if endpoint.starts_with("unix://") => {
                endpoint.trim_start_matches("unix://").to_owned()
            }
            Some(endpoint) if endpoint.starts_with('/') => endpoint.to_owned(),
            Some(endpoint) => {
                return Err(AppError::BadRequest(format!(
                    "`{endpoint}` cannot be reached over the agent connection; \
                     name the daemon's socket path on the far side"
                )));
            }
        };

        // No `record_host_key` here: an agent host's key was pinned
        // at enrollment, in `agent_credential`, not learned from a first
        // connection — there is nothing to write back to `host.ssh_host_key`,
        // and that column is constrained to stay null for this transport.
        return Ok(Arc::new(SshForwarded::new(session, socket)));
    }

    let endpoint = host.docker_endpoint.as_deref().ok_or_else(|| {
        AppError::BadRequest(
            "this host has no docker_endpoint; a daemon faber was not told about \
             is one it should not be talking to"
                .into(),
        )
    })?;

    Ok(Arc::new(LocalSocket::new(endpoint).map_err(fault)?))
}

/// The live session for an agent-transport host, or `Unreachable`.
///
/// There is no dial here and nothing to retry: a daemon that has not
/// connected is not a transient failure this call can wait out, it is the
/// whole of what `Unreachable` means for this transport.
fn agent_session(state: &AppState, host: &Host) -> ApiResult<Arc<SshSession>> {
    state.agents.get(host.id).ok_or_else(|| {
        fault(Fault::Unreachable(format!(
            "no agent daemon is connected for '{}'",
            host.name
        )))
    })
}

/// Stores the fingerprint a first connection saw.
///
/// Only when the column is still null, and never on a mismatch: a host that
/// presents a different key is refused by the verifier above and never reaches
/// here, which is what keeps this from being a trust-on-every-use.
pub(crate) async fn record_host_key(
    state: &AppState,
    host: &Host,
    fingerprint: &str,
) -> ApiResult<()> {
    if host.ssh_host_key.is_some() || fingerprint.is_empty() {
        return Ok(());
    }

    let mut conn = state.db.get().await?;
    diesel::update(
        host::table
            .filter(host::id.eq(host.id))
            .filter(host::ssh_host_key.is_null()),
    )
    .set(host::ssh_host_key.eq(fingerprint))
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "environments.record_host_key"))?;

    Ok(())
}

/// Carries a [`Fault`] across as the status that matches its class.
///
/// The two classes are the whole point of the type: a denial is about the
/// request and the caller repairs it, and an unreachable machine is about the
/// world and the caller retries. Flattening both to 500 would tell a user with
/// a typo in an endpoint that faber is broken.
pub fn fault(fault: Fault) -> AppError {
    match fault {
        Fault::Denied(Denial::NotFound { path }) => {
            AppError::BadRequest(format!("`{path}` was not found on that machine"))
        }
        Fault::Denied(denial) => AppError::BadRequest(denial.to_string()),
        Fault::Unreachable(reason) => AppError::ServiceUnavailable(reason),
    }
}

// ---------------------------------------------------------------------------
// What a session can be told to reach
// ---------------------------------------------------------------------------

/// One environment a user could tag, and the name they would tag it by.
///
/// A "target" in the environment crate's sense is a container, or a host used
/// directly with a root registered. Both appear here under one namespace,
/// because what the user types is a name and not a kind.
pub struct Candidate {
    /// What follows the `@`.
    pub label: String,
    pub kind: &'static str,
    pub host_id: Uuid,
    pub host_name: String,
    pub container_id: Option<Uuid>,
    pub root_path: String,
    /// Operator intent, carried so a picker can show it rather than offering a
    /// name that will refuse at bind.
    pub disabled: bool,
}

/// Everything the caller could tag, with the names to tag them by.
///
/// Names are short where they can be and qualified where they must be: a
/// container's own name unless two hosts carry that name, in which case both
/// become `host/name`. Qualifying only on collision is the difference between
/// a name a user will type and one they will copy — and doing it in one pass
/// here keeps the rule in one place rather than in a picker and a parser that
/// can disagree.
pub async fn candidates(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
) -> ApiResult<Vec<Candidate>> {
    let hosts: Vec<Host> = host::table
        .filter(host::user_id.eq(user_id))
        .order(host::created_at.asc())
        .select(Host::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "environments.candidates.hosts"))?;

    let ids: Vec<Uuid> = hosts.iter().map(|host| host.id).collect();
    let containers: Vec<HostContainer> = if ids.is_empty() {
        Vec::new()
    } else {
        host_container::table
            .filter(host_container::host_id.eq_any(&ids))
            .filter(host_container::user_id.eq(user_id))
            .filter(host_container::unregistered_at.is_null())
            .order(host_container::created_at.asc())
            .select(HostContainer::as_select())
            .load(conn)
            .await
            .map_err(|err| AppError::db(err, "environments.candidates.containers"))?
    };

    let mut found: Vec<Candidate> = Vec::new();

    for host in &hosts {
        // A host with no root is not a place: the path contract needs exactly
        // one rooted namespace per target, and there is nothing here to root.
        if let Some(root_path) = host.root_path.as_deref()
            && host.exec_mode == ExecMode::Direct.as_str()
        {
            found.push(Candidate {
                label: host.name.clone(),
                kind: "host",
                host_id: host.id,
                host_name: host.name.clone(),
                container_id: None,
                root_path: root_path.to_owned(),
                disabled: host.disabled_at.is_some(),
            });
        }
    }

    for container in &containers {
        let Some(host) = hosts.iter().find(|host| host.id == container.host_id) else {
            continue;
        };
        found.push(Candidate {
            label: container
                .name
                .clone()
                .unwrap_or_else(|| container.container_ref.clone()),
            kind: "container",
            host_id: host.id,
            host_name: host.name.clone(),
            container_id: Some(container.id),
            root_path: container.root_path.clone(),
            disabled: host.disabled_at.is_some(),
        });
    }

    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for candidate in &found {
        *counts.entry(candidate.label.clone()).or_default() += 1;
    }
    for candidate in &mut found {
        if counts.get(&candidate.label).copied().unwrap_or(0) > 1 {
            candidate.label = format!("{}/{}", candidate.host_name, candidate.label);
        }
    }

    Ok(found)
}

// ---------------------------------------------------------------------------
// Tagging an environment in a message
// ---------------------------------------------------------------------------

/// Pulls `@name` mentions out of a message, in the order they appear and
/// without repeats.
///
/// An `@` immediately after a word character is not a mention — that is an
/// email address, a handle inside a word, a Rust lifetime in a code fence. The
/// same reasoning bounds what a name may contain: letters, digits, `-`, `_`,
/// `.`, and the `/` that qualifies a name by host.
///
/// A mention that resolves to nothing stays ordinary text. The picker in the
/// client only ever inserts names that exist, so an unresolvable one is either
/// prose or a typo, and refusing to send the message over it would be
/// answering prose with an error.
pub fn mentions(content: &str) -> Vec<String> {
    let characters: Vec<char> = content.chars().collect();
    let mut found: Vec<String> = Vec::new();

    for (index, character) in characters.iter().enumerate() {
        if *character != '@' {
            continue;
        }
        if index > 0 && is_name_char(characters[index - 1]) {
            continue;
        }
        let name: String = characters[index + 1..]
            .iter()
            .take_while(|c| is_name_char(**c))
            .collect();
        // A trailing `.` is sentence punctuation far more often than part of a
        // name, and a name that genuinely ends in one can be tagged by the
        // qualified form.
        let name = name.trim_end_matches('.').to_owned();
        if !name.is_empty() && !found.contains(&name) {
            found.push(name);
        }
    }
    found
}

fn is_name_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | '/')
}

/// Adds every environment a message tagged that the session did not already
/// have, and returns the labels newly added, in the order they were tagged.
///
/// Adding is idempotent per session: tagging an environment that is already
/// bound changes nothing and announces nothing, because it is already true.
/// A label that was bound and then removed stays claimed — the insert
/// conflicts and is left alone — since a transcript recording `label:path`
/// cannot survive a label meaning a second machine.
pub async fn tag_environments(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    session_id: Uuid,
    content: &str,
) -> ApiResult<Vec<String>> {
    let tagged = mentions(content);
    if tagged.is_empty() {
        return Ok(Vec::new());
    }

    let available = candidates(conn, user_id).await?;
    let existing: Vec<String> = session_environment::table
        .filter(session_environment::session_id.eq(session_id))
        .select(session_environment::label)
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "environments.tag.existing"))?;

    let now = now_epoch();
    let mut added = Vec::new();

    for label in tagged {
        if existing.contains(&label) {
            continue;
        }
        let Some(candidate) = available.iter().find(|entry| entry.label == label) else {
            // Prose, not a tag.
            continue;
        };

        let inserted = diesel::insert_into(session_environment::table)
            .values(&NewSessionEnvironment {
                session_id,
                label: &candidate.label,
                host_id: candidate.host_id,
                container_id: candidate.container_id,
                added_at: now,
            })
            .on_conflict_do_nothing()
            .execute(conn)
            .await
            .map_err(|err| AppError::db(err, "environments.tag.insert"))?;

        if inserted > 0 {
            added.push(candidate.label.clone());
        }
    }

    Ok(added)
}

/// What the model is told when environments are added mid-conversation.
///
/// A message in the conversation, not an edit to the system prompt. The prompt
/// sits ahead of every cached byte, so changing it to record something that
/// happened at turn nine would invalidate the whole prefix and rewrite history
/// to claim the environment had been there all along. A turn appended at the
/// end costs nothing cached and says what actually happened, in order.
///
/// **A user turn, though it is not the user speaking.** A system role is what
/// this is, and the lineage would carry one — but Anthropic takes a system
/// prompt only as a top-level field, so a system turn that is not the leading
/// run stays inline in `messages`, where the API accepts `user` and
/// `assistant` and refuses everything else. The turn would be rejected on
/// exactly the request it exists to inform. The `<environments>` tags are what
/// mark it as not the user's prose, and they are what the model reads; the
/// role is what the wire can carry.
pub fn announcement(added: &[String]) -> Option<llm::Message> {
    if added.is_empty() {
        return None;
    }

    let names = added
        .iter()
        .map(|label| format!("'{label}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let noun = if added.len() == 1 {
        "environment"
    } else {
        "environments"
    };

    Some(llm::Message {
        role: llm::Role::User,
        content: vec![llm::ContentBlock::Text {
            text: format!("<environments>User added {names} {noun}</environments>"),
        }],
    })
}

// ---------------------------------------------------------------------------
// Binding a session's environments for a run
// ---------------------------------------------------------------------------

/// What a run got, and what it did not.
pub struct Bound {
    pub registry: Registry,
    /// Labels that are bound to the session but could not be reached this run,
    /// with why. Reported rather than swallowed: one unreachable machine is
    /// not a reason to fail a conversation, and a silently missing target is a
    /// model wondering why its own environment does not exist.
    pub unreachable: Vec<(String, String)>,
}

/// Builds this run's registry from the session's live bindings.
///
/// Bindings are the user's and reach the run as data — nothing the model calls
/// adds one. Each is probed here rather than at the moment it was tagged: a
/// probe is a network round trip, the `POST` that tags an environment has to
/// stay fast, and a machine that was reachable when tagged may not be now.
///
/// The cost of that is paid per turn, and it is worth stating rather than
/// discovering: an ssh environment that has gone away spends the SSH connect
/// timeout — twenty seconds — before every message in that session, and one
/// that has gone away permanently spends it forever. The run reports the
/// failure to the user, which is what makes it fixable, but nothing here backs
/// off. That is the next thing to want, and it needs a place to remember a
/// failure across runs, which is state this layer does not have yet.
pub async fn bind_session(
    state: &AppState,
    user_id: Uuid,
    session_id: Uuid,
    blobs: Arc<dyn Blobs>,
) -> ApiResult<Bound> {
    let mut conn = state.db.get().await?;
    let rows: Vec<SessionEnvironment> = session_environment::table
        .filter(session_environment::session_id.eq(session_id))
        .filter(session_environment::removed_at.is_null())
        .order(session_environment::added_at.asc())
        .select(SessionEnvironment::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "environments.bind.rows"))?;
    drop(conn);

    let mut registry = Registry::new();
    let mut unreachable = Vec::new();

    for row in rows {
        match bind_one(state, user_id, &row, Arc::clone(&blobs)).await {
            Ok(machine) => {
                if let Err(denial) = registry.bind(row.label.clone(), Arc::new(machine)) {
                    unreachable.push((row.label.clone(), denial.to_string()));
                }
            }
            Err(error) => unreachable.push((row.label.clone(), error.to_string())),
        }
    }

    Ok(Bound {
        registry,
        unreachable,
    })
}

async fn bind_one(
    state: &AppState,
    user_id: Uuid,
    row: &SessionEnvironment,
    blobs: Arc<dyn Blobs>,
) -> ApiResult<Machine> {
    let mut conn = state.db.get().await?;
    let host_row: Host = host::table
        .filter(host::id.eq(row.host_id))
        .filter(host::user_id.eq(user_id))
        .select(Host::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "environments.bind.host"))?
        .ok_or(AppError::NotFound)?;

    if host_row.disabled_at.is_some() {
        return Err(AppError::BadRequest("this host is disabled".into()));
    }

    let container = match row.container_id {
        None => None,
        Some(id) => Some(
            host_container::table
                .filter(host_container::id.eq(id))
                .filter(host_container::user_id.eq(user_id))
                .select(HostContainer::as_select())
                .first::<HostContainer>(&mut conn)
                .await
                .optional()
                .map_err(|err| AppError::db(err, "environments.bind.container"))?
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "this environment's container is no longer registered".into(),
                    )
                })?,
        ),
    };
    drop(conn);

    if let Some(container) = container {
        let daemon = reach_daemon(state, user_id, &host_row).await?;
        let root = Root::new(&container.root_path).map_err(|denial| fault(denial.into()))?;
        return DockerTarget::bind(
            row.label.clone(),
            daemon,
            container.container_ref.clone(),
            root,
            blobs,
        )
        .await
        .map_err(fault);
    }

    // Direct execution on the machine itself. It needs a root of its own,
    // and a host without one is not bindable — inventing `/` would hand the
    // agent the whole machine because a field was left empty.
    let root_path = host_row.root_path.clone().ok_or_else(|| {
        AppError::BadRequest(
            "this host has no root_path, so it cannot be used directly; \
             set one, or bind a container on it"
                .into(),
        )
    })?;
    let root = Root::new(&root_path).map_err(|denial| fault(denial.into()))?;

    if host_row.transport == Transport::Ssh.as_str() {
        let session = state.ssh.get(state, user_id, &host_row).await?;
        return SshTarget::bind_session(row.label.clone(), session, root, blobs)
            .await
            .map_err(fault);
    }

    if host_row.transport == Transport::Agent.as_str() {
        let session = agent_session(state, &host_row)?;
        // No fingerprint comes back, and none is written: the session is
        // already authenticated, against a key pinned at enrollment rather
        // than learned here.
        return SshTarget::bind_session(row.label.clone(), session, root, blobs)
            .await
            .map_err(fault);
    }

    LocalTarget::bind(row.label.clone(), root, blobs)
        .await
        .map_err(fault)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mention_is_a_name_after_an_at_that_starts_a_word() {
        assert_eq!(mentions("run it on @build please"), vec!["build"]);
        assert_eq!(mentions("@build"), vec!["build"]);
        assert_eq!(mentions("(@build)"), vec!["build"]);
        // Qualified by host, which is what a colliding name becomes.
        assert_eq!(mentions("@laptop/build"), vec!["laptop/build"]);
    }

    #[test]
    fn an_email_address_is_not_a_mention() {
        // The `@` sits inside a word, which is what separates an address from
        // a tag. Getting this wrong binds an environment because someone
        // pasted a contact.
        assert!(mentions("mail me at someone@example.com").is_empty());
        assert!(mentions("x@y").is_empty());
    }

    #[test]
    fn a_repeat_is_tagged_once_and_punctuation_stays_out_of_the_name() {
        assert_eq!(mentions("@build and @build again"), vec!["build"]);
        assert_eq!(mentions("deploy to @staging."), vec!["staging"]);
    }

    #[test]
    fn the_announcement_is_a_conversation_turn_and_never_a_leading_one() {
        assert!(announcement(&[]).is_none());

        let one = announcement(&["build".to_owned()]).expect("one environment");
        // Not a system turn, however much it reads like one: Anthropic hoists
        // only a *leading* system run into its top-level field, so this one
        // would go inline in `messages` and be refused. The tags carry what
        // the role cannot.
        assert!(matches!(one.role, llm::Role::User));
        assert_eq!(
            one.text(),
            "<environments>User added 'build' environment</environments>"
        );

        // Several in one turn become one message, not several: two separate
        // notes would say the same thing twice, and one is what a reader and a
        // model both want.
        let two =
            announcement(&["build".to_owned(), "staging".to_owned()]).expect("two environments");
        assert_eq!(
            two.text(),
            "<environments>User added 'build', 'staging' environments</environments>"
        );
    }
}
