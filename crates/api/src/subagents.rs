//! The agent tools: how a session's agent starts other agents and talks to
//! them.
//!
//! The host grants these to every project session; no plugin supplies them,
//! so they are outside the project's bindings and their snapshot. A spawn
//! creates a child session ([`crate::children`]) from one of the project's
//! agent definitions ([`crate::definitions`]), puts the prompt in its inbox
//! ([`crate::inbox`]) and returns at once: the child runs on its own, and
//! what it says comes back as a message.
//!
//! A session reaches only the children it spawned. A child is granted only
//! `agent__send`, to the agent that started it, so the tree is one level
//! deep and a child can answer its parent while it works.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use harness::mapping::ToolResult;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agent_limits;
use crate::children::{self, Spawn, SpawnError};
use crate::db::DbPool;
use crate::definitions::{self, Definition};
use crate::inbox::{self, Dedupe, Inbox, PutError, Refused, Said};
use crate::models::session::Session;
use crate::run::{InterruptRegistry, RunRegistry};
use crate::said::Origin;

pub const SPAWN: &str = "agent__spawn";
pub const SEND: &str = "agent__send";
pub const STOP: &str = "agent__stop";
pub const LIST: &str = "agent__list";

/// Characters of a prompt or a message one agent sends another.
const MESSAGE_CHARS: usize = 64_000;

/// Characters of a child's final message its parent is sent. A longer one
/// is cut, and the report says so.
const REPORT_CHARS: usize = 16_000;

/// Characters of a failure a report quotes.
const FAILURE_CHARS: usize = 500;

/// The dedupe scope of what one agent sends another.
const AGENT: &str = "agent";

/// The runs a parent stopped with `agent__stop`, in this process, until
/// each ends: such a run reports nothing, since its parent already knows.
pub type Stops = Arc<Mutex<HashSet<Uuid>>>;

/// How a child's run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    Finished,
    /// With what went wrong.
    Failed(String),
    Stopped,
    /// The process restarted while it ran.
    Lost,
}

/// Characters of the line `agent__list` gives on what a child is doing.
const DOING_CHARS: usize = 160;

/// Added to the head of a session an agent spawned, after its definition's
/// prompt.
pub const SUBAGENT_PROMPT: &str = "You are a subagent: another agent working in this project \
    started you to do the task in its first message, and messages with `source=\"agent\"` are \
    from it. Nobody reads what you write while you work. When that agent asks you something, \
    answer with `agent__send` to `parent`, then carry on. When you finish, your final message is \
    delivered to that agent as your report, so make it brief and complete: what you found or \
    did, and anything it needs to know to carry on.";

/// What `agent__send` takes, from a child, for the agent that started it.
pub const PARENT: &str = "parent";

/// Whether `session`'s agent is granted the tools: a session in a project
/// that no agent spawned.
pub fn granted(session: &Session) -> bool {
    session.project_id.is_some() && session.parent_session_id.is_none()
}

/// What the head of a session spawned as `definition` adds to the core
/// prompt. A definition that is gone, or the built-in, adds no prompt of its
/// own.
pub fn head(definition: Option<&Definition>) -> String {
    match definition.map(|definition| definition.prompt.trim()) {
        Some(prompt) if !prompt.is_empty() => format!("{prompt}\n\n{SUBAGENT_PROMPT}"),
        _ => SUBAGENT_PROMPT.to_owned(),
    }
}

/// The project's definitions as `agent__spawn` describes them: a line each.
pub fn listing(definitions: &[Definition]) -> String {
    definitions
        .iter()
        .map(|definition| {
            // One line each, whatever the owner typed.
            let description = definition
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            format!("- {}: {description}", definition.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The four tools, with `listing` as the agent types `agent__spawn` offers.
pub fn tools(listing: &str) -> Vec<llm::ToolDef> {
    let id = json!({ "type": "string", "description": "The agent's id, as `agent__spawn` returned it." });
    vec![
        llm::ToolDef {
            name: SPAWN.to_owned(),
            description: format!(
                "Starts another agent on a task and returns its id at once, without waiting for \
                 it. The agent works on its own, in a conversation of its own that starts empty: \
                 it knows nothing of this one, so `prompt` must say everything it needs. When it \
                 finishes, its report arrives as a message from it; do not wait or poll for it. \
                 It may also message you while it works, such as to answer you; its report is the \
                 message that says how it ended. \
                 Spawn several in one turn to have them work at the same time; past the \
                 project's limit on agents running at once, the rest are `queued` and start as \
                 others finish.\n\n\
                 Agent types:\n{listing}\n\n\
                 Types may have been added since; the error for an unknown type lists them."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "One of the agent types above." },
                    "prompt": { "type": "string", "description": "The task, complete in itself." },
                    "name": {
                        "type": "string",
                        "description": "A short name to tell this agent from the others, such as what it is working on.",
                    },
                },
                "required": ["type", "prompt"],
            }),
        },
        llm::ToolDef {
            name: SEND.to_owned(),
            description: "Sends a message to an agent you started. One that has finished carries \
                on from where it left off, with everything it did still in view; one that is \
                working reads the message after its current step, and answers, if it does, with \
                a message of its own. End your turn to wait for the answer: it starts your next."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "id": id, "message": { "type": "string" } },
                "required": ["id", "message"],
            }),
        },
        llm::ToolDef {
            name: STOP.to_owned(),
            description: "Stops an agent you started: interrupts what it is doing and drops the \
                messages it has not read. It keeps what it did, and `agent__send` starts it again."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "id": id },
                "required": ["id"],
            }),
        },
        llm::ToolDef {
            name: LIST.to_owned(),
            description: "Lists the agents you started: each one's id, type, name, status \
                (`running`, `queued` for its turn to run, `idle` once it has finished, \
                `failed`, `stopped`, or `closed`), and the last thing it said or did."
                .to_owned(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
    ]
}

/// The one tool a session an agent spawned is granted: `agent__send`, to
/// the agent that started it.
pub fn parent_tools() -> Vec<llm::ToolDef> {
    vec![llm::ToolDef {
        name: SEND.to_owned(),
        description: "Sends a message to the agent that started you, while you work: to answer \
            what it asked, or to tell it something it needs before your report. It reads the \
            message after its current step. Your final message is still your report; do not \
            send it this way too."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "`parent`, or the id of the agent that started you.",
                },
                "message": { "type": "string" },
            },
            "required": ["id", "message"],
        }),
    }]
}

/// The tools of one run of a session.
pub struct AgentTools {
    pub db: DbPool,
    pub inbox: Arc<Inbox>,
    pub runs: RunRegistry,
    pub interrupts: InterruptRegistry,
    pub stops: Stops,
    /// The session whose agent calls them.
    pub session: Uuid,
    /// The session that spawned it, for a child: then only `agent__send`
    /// answers, and only to it.
    pub parent: Option<Uuid>,
    pub project: Uuid,
    /// The project's owner, whose models the children run on.
    pub owner: Uuid,
    /// The run that calls them.
    pub run: Uuid,
    /// What this run runs on: a child's, unless its definition says otherwise.
    pub model_alias: String,
    pub thinking_effort: Option<String>,
    /// The operator's ceilings on the project's agents.
    pub limits: agent_limits::Ceilings,
}

fn ok(content: impl Into<String>) -> ToolResult {
    ToolResult {
        content: content.into(),
        is_error: false,
    }
}

fn refused(content: impl Into<String>) -> ToolResult {
    ToolResult {
        content: content.into(),
        is_error: true,
    }
}

fn unavailable(error: impl std::fmt::Display) -> ToolResult {
    tracing::warn!(%error, "agent tool call failed");
    refused("The agents could not be reached just now; try again.")
}

/// A required string argument, trimmed and not empty.
fn text<'a>(input: &'a Value, name: &str) -> Result<&'a str, ToolResult> {
    match input.get(name).and_then(Value::as_str).map(str::trim) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(refused(format!("`{name}` is required"))),
    }
}

fn message<'a>(input: &'a Value, name: &str) -> Result<&'a str, ToolResult> {
    let value = text(input, name)?;
    if value.chars().count() > MESSAGE_CHARS {
        return Err(refused(format!(
            "`{name}` must be {MESSAGE_CHARS} characters or fewer"
        )));
    }
    Ok(value)
}

// The derive expands each field as `field: field`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::sql_types::{BigInt, Jsonb, Uuid as SqlUuid};
    use uuid::Uuid;

    #[derive(diesel::QueryableByName)]
    pub struct Payload {
        #[diesel(sql_type = Jsonb)]
        pub payload: serde_json::Value,
    }

    #[derive(diesel::QueryableByName)]
    pub struct RunId {
        #[diesel(sql_type = SqlUuid)]
        pub id: Uuid,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Count {
        #[diesel(sql_type = BigInt)]
        pub count: i64,
    }
}

/// The last thing a message row's author said or did, on one line.
pub(crate) fn doing(payload: &Value) -> Option<String> {
    let block = payload.get("content")?.as_array()?.last()?;
    let line = match block.get("type")?.as_str()? {
        "tool_use" => format!("calling {}", block.get("name")?.as_str()?),
        "text" => block.get("text")?.as_str()?.to_owned(),
        _ => return None,
    };
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    let cut: String = line.chars().take(DOING_CHARS).collect();
    Some(if cut.len() < line.len() {
        format!("{cut}…")
    } else {
        cut
    })
}

/// The text of a message row, if an assistant wrote it.
fn assistant_text(payload: &Value) -> Option<String> {
    if payload.get("role")?.as_str()? != "assistant" {
        return None;
    }
    let text = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text")?.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(text)
}

/// What a parent is told when a child's run ends: how it ended, whether
/// messages `waiting` for the child start it again, then the child's final
/// message.
fn report_text(ended: &Ended, said: Option<&str>, waiting: bool) -> String {
    let mut how = match ended {
        Ended::Finished => "The agent finished.".to_owned(),
        Ended::Stopped => "The agent was stopped before it finished.".to_owned(),
        Ended::Lost if waiting => "The agent was interrupted by a restart before it finished. \
             It keeps what it had done."
            .to_owned(),
        Ended::Lost => format!(
            "The agent was interrupted by a restart before it finished. It keeps what it had \
             done; send it a message with {SEND} to have it carry on."
        ),
        Ended::Failed(error) => {
            let error: String = error.chars().take(FAILURE_CHARS).collect();
            format!("The agent failed: {error}")
        }
    };
    if waiting {
        how.push_str(
            "\nIt has messages it had not finished with, so it is starting again on them and \
             will report again when it ends.",
        );
    }
    let said = said.map(str::trim).filter(|said| !said.is_empty());
    let Some(said) = said else {
        return format!("{how}\nIt said nothing.");
    };
    let total = said.chars().count();
    if total <= REPORT_CHARS {
        return format!("{how}\nIts report:\n\n{said}");
    }
    let cut: String = said.chars().take(REPORT_CHARS).collect();
    format!(
        "{how}\nIts report:\n\n{cut}\n\n[The report was cut here, at {REPORT_CHARS} of {total} \
         characters. Ask the agent for the rest with {SEND}.]"
    )
}

/// Stops `session`'s agent: drops what it has not read, and interrupts its
/// runs under way in this process. With `stops`, as the one that spawned it
/// would, and those runs report nothing; without, as anyone else would, and
/// its parent is told it was stopped. Returns how many runs it interrupted.
pub(crate) async fn halt(
    conn: &mut AsyncPgConnection,
    interrupts: &InterruptRegistry,
    stops: Option<&Stops>,
    session: Uuid,
) -> Result<usize, diesel::result::Error> {
    // What it has not read goes first, so the end of the run being stopped
    // finds nothing to start another with.
    conn.transaction::<(), diesel::result::Error, _>(|conn| {
        async move {
            inbox::lock(conn, session).await?;
            inbox::clear(conn, session).await
        }
        .scope_boxed()
    })
    .await?;
    let live: Vec<rows::RunId> = diesel::sql_query(
        "SELECT r.id FROM run r JOIN thread t ON t.id = r.thread_id \
         WHERE t.session_id = $1 AND r.completed_at IS NULL",
    )
    .bind::<SqlUuid, _>(session)
    .load(conn)
    .await?;
    let stopped = live
        .iter()
        .filter(|run| {
            // Marked first: the run may end, and look for the mark, the
            // moment it is interrupted.
            if let Some(stops) = stops {
                stops.lock().expect("agent stops poisoned").insert(run.id);
            }
            let raised = crate::run::raise_interrupt(interrupts, run.id);
            if !raised && let Some(stops) = stops {
                stops.lock().expect("agent stops poisoned").remove(&run.id);
            }
            raised
        })
        .count();
    Ok(stopped)
}

/// Stops every agent under `session`, and what they spawned: for a parent
/// whose run was stopped, or that was closed, so nothing keeps spending for
/// it. Those that were running report nothing; the parent asked for this.
pub async fn stop_under(
    db: &DbPool,
    interrupts: &InterruptRegistry,
    stops: &Stops,
    session: Uuid,
) -> usize {
    let Ok(mut conn) = db.get().await else {
        tracing::warn!(%session, "its agents were not stopped: no database");
        return 0;
    };
    let under = match children::descendants(&mut conn, session).await {
        Ok(under) => under,
        Err(error) => {
            tracing::warn!(%session, %error, "its agents were not stopped");
            return 0;
        }
    };
    let mut stopped = 0;
    for child in under {
        match halt(&mut conn, interrupts, Some(stops), child).await {
            Ok(count) => stopped += count,
            Err(error) => tracing::warn!(%session, %child, %error, "an agent was not stopped"),
        }
    }
    stopped
}

/// A run of `session` ended: if an agent spawned the session, its parent is
/// sent a report, as a message from the child. An idle parent starts a run
/// with it; a running one reads it after its current step.
///
/// The run's id is the dedupe id, so a run reports once however often this
/// is reached. A run its parent stopped with `agent__stop` reports nothing.
pub async fn report(
    db: &DbPool,
    inbox: &Inbox,
    stops: &Stops,
    session: Uuid,
    run: Uuid,
    ended: Ended,
) {
    let by_parent = stops.lock().expect("agent stops poisoned").remove(&run);
    let Ok(mut conn) = db.get().await else {
        return;
    };
    use crate::schema::session as table;
    use diesel::SelectableHelper;
    let child: Option<Session> = match table::table
        .filter(table::id.eq(session))
        .filter(table::parent_session_id.is_not_null())
        .select(Session::as_select())
        .first(&mut conn)
        .await
        .optional()
    {
        Ok(child) => child,
        Err(error) => return tracing::warn!(%session, %error, "a child's report was not sent"),
    };
    let Some((child, parent)) =
        child.and_then(|child| Some((child.clone(), child.parent_session_id?)))
    else {
        return;
    };
    if by_parent {
        return;
    }
    let last: Option<rows::Payload> = match diesel::sql_query(
        "SELECT payload FROM transcript WHERE run_id = $1 AND kind = $2 \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind::<SqlUuid, _>(run)
    .bind::<Text, _>(crate::compact::KIND_MESSAGE)
    .get_result(&mut conn)
    .await
    .optional()
    {
        Ok(last) => last,
        Err(error) => {
            // Still reported: how it ended is worth more than nothing.
            tracing::warn!(%session, %error, "a child's final message could not be read");
            None
        }
    };
    // A run takes the messages it starts with out of the inbox; any left are
    // ones that came during it, read or not, and they start its next run.
    // Read before the inbox is kicked at the run's end, which leaves them
    // until that run commits.
    let waiting = match diesel::sql_query(
        "SELECT count(*) AS count FROM session_inbox WHERE session_id = $1",
    )
    .bind::<SqlUuid, _>(session)
    .get_result::<rows::Count>(&mut conn)
    .await
    {
        Ok(row) => row.count > 0,
        Err(error) => {
            tracing::warn!(%session, %error, "a child's waiting messages could not be counted");
            false
        }
    };
    drop(conn);
    let said = last.and_then(|row| assistant_text(&row.payload));
    let name = child.agent_name.as_deref().or(child.agent_type.as_deref());
    let message = Said {
        text: report_text(&ended, said.as_deref(), waiting),
        origin: Origin::agent(child.id, name),
    };
    let dedupe = Dedupe {
        scope: AGENT,
        id: &run.to_string(),
    };
    match inbox.put(parent, message, Some(dedupe)).await {
        // A parent that is closed or gone has nobody to tell.
        Ok(()) | Err(PutError::Refused(Refused::Closed | Refused::Missing)) => {}
        Err(error) => {
            tracing::warn!(%session, %parent, ?error, "a child's report was not delivered")
        }
    }
}

impl AgentTools {
    /// Answers a call of one of the tools, or `None` for any other name.
    pub async fn invoke(&self, name: &str, input: &Value) -> Option<ToolResult> {
        if let Some(parent) = self.parent {
            let answer = match name {
                SEND => self.tell(parent, input).await,
                _ => return None,
            };
            return Some(answer.unwrap_or_else(|refusal| refusal));
        }
        let answer = match name {
            SPAWN => self.spawn(input).await,
            SEND => self.send(input).await,
            STOP => self.stop(input).await,
            LIST => self.list().await,
            _ => return None,
        };
        Some(answer.unwrap_or_else(|refusal| refusal))
    }

    /// The child `input.id` names, if this session spawned it. One it did
    /// not spawn and one that does not exist read the same.
    async fn child(
        &self,
        conn: &mut AsyncPgConnection,
        input: &Value,
    ) -> Result<Session, ToolResult> {
        let id = text(input, "id")?;
        let unknown = || refused(format!("You have started no agent `{id}`."));
        let id: Uuid = id.parse().map_err(|_| unknown())?;
        use crate::schema::session;
        use diesel::SelectableHelper;
        session::table
            .filter(session::id.eq(id))
            .filter(session::parent_session_id.eq(self.session))
            .select(Session::as_select())
            .first(conn)
            .await
            .optional()
            .map_err(unavailable)?
            .ok_or_else(unknown)
    }

    async fn put(&self, child: Uuid, text: &str) -> Result<(), ToolResult> {
        let said = Said {
            text: text.to_owned(),
            origin: Origin::agent(self.session, None),
        };
        self.inbox
            .put(child, said, None)
            .await
            .map_err(|error| match error {
                PutError::Refused(Refused::Full) => refused(
                    "That agent has too many messages it has not read yet; wait for it to catch up.",
                ),
                PutError::Refused(Refused::Closed | Refused::Missing) => {
                    refused("That agent has been closed.")
                }
                PutError::Unavailable => unavailable("the inbox could not be reached"),
            })
    }

    /// The definition a spawn names, and the model a child spawned as it
    /// runs on: the definition's, or this run's.
    async fn definition(&self, agent_type: &str) -> Result<(Definition, String), ToolResult> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let found = definitions::find(&mut conn, self.project, agent_type)
            .await
            .map_err(unavailable)?;
        let Some(definition) = found else {
            let all = definitions::list(&mut conn, self.project)
                .await
                .map_err(unavailable)?;
            return Err(refused(format!(
                "There is no agent type `{agent_type}`. The agent types are:\n{}",
                listing(&all)
            )));
        };
        // A definition's model may have been deleted since it was saved. A
        // child with a model its owner does not have would never run, and
        // say nothing.
        let model: String = match definition.model_alias.clone() {
            Some(alias) => {
                use crate::schema::models;
                let known: Option<Uuid> = models::table
                    .filter(models::user_id.eq(self.owner))
                    .filter(models::alias.eq(&alias))
                    .select(models::id)
                    .first(&mut conn)
                    .await
                    .optional()
                    .map_err(unavailable)?;
                if known.is_none() {
                    return Err(refused(format!(
                        "The agent type `{agent_type}` runs on the model `{alias}`, which is no \
                         longer there. The project's owner has to fix the agent type."
                    )));
                }
                alias
            }
            None => self.model_alias.clone(),
        };
        Ok((definition, model))
    }

    async fn spawn(&self, input: &Value) -> Result<ToolResult, ToolResult> {
        let agent_type = text(input, "type")?;
        let prompt = message(input, "prompt")?;
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let (definition, model) = self.definition(agent_type).await?;
        // After what the child is made from: the transaction borrows both.
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let limit = agent_limits::children_cap(&mut conn, self.project, self.limits)
            .await
            .map_err(unavailable)?;
        let spawn = Spawn {
            run: Some(self.run),
            agent_type: &definition.name,
            name,
            model_alias: Some(&model),
            thinking_effort: self.thinking_effort.as_deref(),
            plugins: definition.plugins.as_deref(),
            limit: Some(limit),
        };
        let parent = self.session;
        let child = conn
            .transaction::<Session, SpawnError, _>(|conn| {
                async move { children::spawn(conn, parent, spawn).await }.scope_boxed()
            })
            .await
            .map_err(|error| match error {
                SpawnError::BadName => refused(format!(
                    "`name` must be {} characters or fewer",
                    children::NAME_CHARS
                )),
                SpawnError::TooMany(limit) => refused(format!(
                    "You already have {limit} agents, as many as a session may have. Give one of \
                     them the task with `agent__send` instead; an agent that has finished carries \
                     on with everything it did still in view."
                )),
                SpawnError::ParentClosed | SpawnError::NoParent => {
                    refused("This session is closed and cannot start agents.")
                }
                SpawnError::Db(error) => unavailable(error),
            })?;
        drop(conn);
        self.put(child.id, prompt).await?;
        // The put started its run, unless the project's limit holds it back.
        let status = if crate::run::is_running(&self.runs, child.id) {
            "running"
        } else {
            "queued"
        };
        Ok(ok(
            json!({ "id": child.id, "type": definition.name, "status": status }).to_string(),
        ))
    }

    async fn send(&self, input: &Value) -> Result<ToolResult, ToolResult> {
        let text = message(input, "message")?;
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let child = self.child(&mut conn, input).await?;
        drop(conn);
        if child.closed_at.is_some() {
            return Err(refused("That agent has been closed."));
        }
        let running = crate::run::is_running(&self.runs, child.id);
        self.put(child.id, text).await?;
        Ok(ok(if running {
            "Sent. The agent is working and reads it after its current step."
        } else {
            "Sent. The agent carries on from where it left off."
        }))
    }

    /// A child's message to `parent`, the session that spawned it: as from
    /// the child, by its name, the way its report arrives.
    async fn tell(&self, parent: Uuid, input: &Value) -> Result<ToolResult, ToolResult> {
        let id = text(input, "id")?;
        let named = id.eq_ignore_ascii_case(PARENT) || id.parse::<Uuid>() == Ok(parent);
        if !named {
            return Err(refused(format!(
                "You can message only the agent that started you: give `id` as `{PARENT}`."
            )));
        }
        let text = message(input, "message")?;
        let mut conn = self.db.get().await.map_err(unavailable)?;
        use crate::schema::session;
        let (name, agent_type): (Option<String>, Option<String>) = session::table
            .filter(session::id.eq(self.session))
            .select((session::agent_name, session::agent_type))
            .first(&mut conn)
            .await
            .map_err(unavailable)?;
        drop(conn);
        let said = Said {
            text: text.to_owned(),
            origin: Origin::agent(self.session, name.as_deref().or(agent_type.as_deref())),
        };
        let running = crate::run::is_running(&self.runs, parent);
        self.inbox
            .put(parent, said, None)
            .await
            .map_err(|error| match error {
                PutError::Refused(Refused::Full) => refused(
                    "The agent that started you has too many messages it has not read yet; \
                     wait, or put what you have in your report.",
                ),
                PutError::Refused(Refused::Closed | Refused::Missing) => {
                    refused("The agent that started you has been closed.")
                }
                PutError::Unavailable => unavailable("the inbox could not be reached"),
            })?;
        Ok(ok(if running {
            "Sent. The agent that started you reads it after its current step."
        } else {
            "Sent. The agent that started you is idle, and the message starts its next turn."
        }))
    }

    async fn stop(&self, input: &Value) -> Result<ToolResult, ToolResult> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let child = self.child(&mut conn, input).await?;
        let stopped = halt(&mut conn, &self.interrupts, Some(&self.stops), child.id)
            .await
            .map_err(unavailable)?;
        Ok(ok(if stopped > 0 {
            "Stopping. It keeps what it has done so far."
        } else {
            "It was not running."
        }))
    }

    async fn list(&self) -> Result<ToolResult, ToolResult> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let spawned = crate::agent_status::of(&mut conn, &self.runs, self.session)
            .await
            .map_err(unavailable)?;
        drop(conn);
        let agents: Vec<Value> = spawned
            .into_iter()
            .map(|child| {
                let status = if child.closed {
                    json!("closed")
                } else {
                    json!(child.status)
                };
                let mut agent = json!({
                    "id": child.id,
                    "type": child.agent_type,
                    "status": status,
                });
                if let Some(name) = &child.name {
                    agent["name"] = json!(name);
                }
                if let Some(line) = child.activity {
                    agent["last"] = json!(line);
                }
                agent
            })
            .collect();
        if agents.is_empty() {
            return Ok(ok("You have started no agents."));
        }
        Ok(ok(Value::Array(agents).to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::testing::FakeLauncher;
    use crate::plugins::daemon::fixture::Fixture;

    fn tools_of(fixture: &Fixture, session: Uuid) -> (AgentTools, Arc<FakeLauncher>) {
        let inbox = Arc::new(Inbox::new(fixture.pool.clone()));
        let launcher = Arc::new(FakeLauncher::default());
        inbox.attach(launcher.clone());
        let tools = AgentTools {
            db: fixture.pool.clone(),
            inbox,
            runs: Default::default(),
            interrupts: Default::default(),
            stops: Default::default(),
            session,
            parent: None,
            project: fixture.project,
            owner: fixture.user,
            run: Uuid::now_v7(),
            model_alias: "fast".into(),
            thinking_effort: Some("low".into()),
            limits: agent_limits::Ceilings::default(),
        };
        (tools, launcher)
    }

    async fn call(tools: &AgentTools, name: &str, input: Value) -> ToolResult {
        tools.invoke(name, &input).await.expect("one of the tools")
    }

    /// The id a successful spawn answered with.
    fn spawned(result: &ToolResult) -> Uuid {
        assert!(!result.is_error, "{}", result.content);
        let answer: Value = serde_json::from_str(&result.content).unwrap();
        answer["id"].as_str().unwrap().parse().unwrap()
    }

    async fn run_of(fixture: &Fixture) -> Uuid {
        // `spawned_by_run_id` references a run: give the parent one.
        let mut conn = fixture.pool.get().await.unwrap();
        let thread = Uuid::now_v7();
        let run = Uuid::now_v7();
        diesel::sql_query("INSERT INTO thread (id, session_id, created_at) VALUES ($1, $2, 1)")
            .bind::<SqlUuid, _>(thread)
            .bind::<SqlUuid, _>(fixture.sessions[0])
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::sql_query("INSERT INTO run (id, thread_id, created_at) VALUES ($1, $2, 1)")
            .bind::<SqlUuid, _>(run)
            .bind::<SqlUuid, _>(thread)
            .execute(&mut conn)
            .await
            .unwrap();
        run
    }

    #[test]
    fn the_tools_are_for_a_project_session_no_agent_spawned() {
        let names: Vec<String> = tools("- general-purpose: x")
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, [SPAWN, SEND, STOP, LIST]);
        let described = &tools("- reviewer: Reviews a change.")[0].description;
        assert!(described.contains("- reviewer: Reviews a change."));

        let general = Definition {
            name: definitions::GENERAL_PURPOSE.into(),
            description: "An agent\nlike you.".into(),
            prompt: String::new(),
            model_alias: None,
            plugins: None,
            updated_at: None,
        };
        assert_eq!(
            listing(std::slice::from_ref(&general)),
            "- general-purpose: An agent like you."
        );
        // A child's head is its definition's prompt, then what a subagent is.
        assert_eq!(head(Some(&general)), SUBAGENT_PROMPT);
        assert_eq!(head(None), SUBAGENT_PROMPT);
        let reviewer = Definition {
            prompt: "You review code.\n".into(),
            ..general
        };
        assert_eq!(
            head(Some(&reviewer)),
            format!("You review code.\n\n{SUBAGENT_PROMPT}")
        );

        assert_eq!(
            doing(&json!({ "content": [{ "type": "text", "text": " reading\nthe  diff " }] })),
            Some("reading the diff".to_owned())
        );
        assert_eq!(
            doing(&json!({ "content": [
                { "type": "text", "text": "let me look" },
                { "type": "tool_use", "name": "environments__exec" },
            ] })),
            Some("calling environments__exec".to_owned())
        );
        assert_eq!(doing(&json!({ "content": [] })), None);
    }

    #[test]
    fn a_report_says_how_the_run_ended_and_what_the_child_said() {
        assert_eq!(
            report_text(&Ended::Finished, Some(" all green \n"), false),
            "The agent finished.\nIts report:\n\nall green"
        );
        assert_eq!(
            report_text(
                &Ended::Failed("the provider answered 529".into()),
                None,
                false
            ),
            "The agent failed: the provider answered 529\nIt said nothing."
        );
        assert_eq!(
            report_text(&Ended::Stopped, Some("  "), false),
            "The agent was stopped before it finished.\nIt said nothing."
        );
        let long = "é".repeat(REPORT_CHARS + 5);
        let cut = report_text(&Ended::Finished, Some(&long), false);
        assert!(cut.contains(&format!(
            "{} of {} characters",
            REPORT_CHARS,
            REPORT_CHARS + 5
        )));
        assert_eq!(cut.matches('é').count(), REPORT_CHARS);

        let said = json!({ "role": "assistant", "content": [
            { "type": "thinking", "thinking": "hm" },
            { "type": "text", "text": "done" },
        ] });
        assert_eq!(assistant_text(&said).as_deref(), Some("done"));
        assert_eq!(
            assistant_text(&json!({ "role": "user", "content": [] })),
            None
        );
    }

    /// A finished run of `session` whose last message is `said`.
    async fn ran(fixture: &Fixture, session: Uuid, said: &str) -> Uuid {
        let mut conn = fixture.pool.get().await.unwrap();
        let thread: Uuid = crate::schema::thread::table
            .filter(crate::schema::thread::session_id.eq(session))
            .select(crate::schema::thread::id)
            .first(&mut conn)
            .await
            .unwrap();
        let run = Uuid::now_v7();
        diesel::sql_query("INSERT INTO run (id, thread_id, created_at) VALUES ($1, $2, 1)")
            .bind::<SqlUuid, _>(run)
            .bind::<SqlUuid, _>(thread)
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::sql_query(
            "INSERT INTO transcript (id, run_id, seq, kind, payload, created_at) \
             VALUES ($1, $2, 1, 'message', $3, 1)",
        )
        .bind::<SqlUuid, _>(Uuid::now_v7())
        .bind::<SqlUuid, _>(run)
        .bind::<diesel::sql_types::Jsonb, _>(
            json!({ "role": "assistant", "content": [{ "type": "text", "text": said }] }),
        )
        .execute(&mut conn)
        .await
        .unwrap();
        run
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_child_s_run_reports_to_its_parent_once() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let child = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go", "name": "scout" }),
            )
            .await,
        );
        let (db, inbox, stops) = (&fixture.pool, &tools.inbox, &tools.stops);

        // The parent ended its turn; the child finishing starts it again,
        // with the report as input, from that child.
        let run = ran(&fixture, child, "found three").await;
        launcher.end(child);
        report(db, inbox, stops, child, run, Ended::Finished).await;
        assert_eq!(
            launcher.launched()[1],
            (
                parent,
                "The agent finished.\nIts report:\n\nfound three".to_owned()
            )
        );
        let said = launcher.said.lock().unwrap().last().cloned().unwrap();
        assert_eq!(said.origin, Origin::agent(child, Some("scout")));
        // The same run does not report twice.
        launcher.end(parent);
        report(db, inbox, stops, child, run, Ended::Finished).await;
        assert_eq!(launcher.launched().len(), 2);

        // A failure is reported as one, and a parent still running keeps it
        // waiting in its inbox to read mid-run.
        let failed = ran(&fixture, child, "").await;
        launcher.end(parent);
        report(
            db,
            inbox,
            stops,
            child,
            failed,
            Ended::Failed("no credit".into()),
        )
        .await;
        assert_eq!(
            launcher.launched()[2].1,
            "The agent failed: no credit\nIt said nothing."
        );
        let during = inbox::RunInbox::new(Arc::clone(inbox), parent);
        let again = ran(&fixture, child, "and two more").await;
        report(db, inbox, stops, child, again, Ended::Finished).await;
        assert_eq!(launcher.launched().len(), 3);
        assert_eq!(
            during.take().await[0].text,
            "The agent finished.\nIts report:\n\nand two more"
        );

        // A run its parent stopped says nothing; a session nobody spawned
        // has nobody to tell.
        let stopped = ran(&fixture, child, "half").await;
        stops.lock().unwrap().insert(stopped);
        report(db, inbox, stops, child, stopped, Ended::Stopped).await;
        report(db, inbox, stops, parent, tools.run, Ended::Finished).await;
        assert!(during.take().await.is_empty());
        assert!(stops.lock().unwrap().is_empty());
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn children_are_not_granted_the_tools() {
        let fixture = Fixture::new(1).await;
        let (mut tools, _) = tools_of(&fixture, fixture.sessions[0]);
        tools.run = run_of(&fixture).await;
        let child = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go" }),
            )
            .await,
        );
        let mut conn = fixture.pool.get().await.unwrap();
        let parent =
            crate::access::authorize_session(&mut conn, fixture.user, fixture.sessions[0]).await;
        assert!(granted(&parent.unwrap()));
        let child = crate::access::authorize_session(&mut conn, fixture.user, child).await;
        assert!(!granted(&child.unwrap()));
        drop(conn);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_child_messages_the_agent_that_started_it_and_no_other() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, _) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let child = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go", "name": "scout" }),
            )
            .await,
        );
        let (mut mine, launcher) = tools_of(&fixture, child);
        mine.parent = Some(parent);
        let names: Vec<String> = parent_tools().into_iter().map(|tool| tool.name).collect();
        assert_eq!(names, [SEND]);
        // A child has only the one tool.
        for name in [SPAWN, STOP, LIST] {
            assert!(mine.invoke(name, &json!({})).await.is_none(), "{name}");
        }

        // Nobody but its parent: not itself, not a stranger.
        for id in [child.to_string(), Uuid::now_v7().to_string(), "lead".into()] {
            let result = call(&mine, SEND, json!({ "id": id, "message": "hi" })).await;
            assert!(result.is_error, "{id}");
            assert!(result.content.contains(PARENT), "{}", result.content);
        }
        assert!(launcher.launched().is_empty());

        // An idle parent starts a turn with it, from the child by its name.
        let sent = call(
            &mine,
            SEND,
            json!({ "id": "parent", "message": "two so far" }),
        )
        .await;
        assert!(!sent.is_error, "{}", sent.content);
        assert_eq!(launcher.launched(), [(parent, "two so far".to_owned())]);
        let said = launcher.said.lock().unwrap().last().cloned().unwrap();
        assert_eq!(said.origin, Origin::agent(child, Some("scout")));

        // A parent at work reads it mid-run; its id names it as well.
        let during = inbox::RunInbox::new(Arc::clone(&mine.inbox), parent);
        let sent = call(
            &mine,
            SEND,
            json!({ "id": parent.to_string(), "message": "three now" }),
        )
        .await;
        assert!(!sent.is_error, "{}", sent.content);
        assert_eq!(launcher.launched().len(), 1);
        assert_eq!(during.take().await[0].text, "three now");
        fixture.cleanup().await;
    }

    #[test]
    fn a_run_lost_to_a_restart_says_how_to_carry_on() {
        let text = report_text(&Ended::Lost, Some("half done"), false);
        assert!(text.starts_with("The agent was interrupted by a restart"));
        assert!(text.contains(SEND));
        assert!(text.ends_with("half done"));
        // With messages waiting it carries on by itself: no advice to send one.
        let text = report_text(&Ended::Lost, Some("half done"), true);
        assert!(!text.contains(SEND), "{text}");
        assert!(text.contains("starting again"), "{text}");
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_report_says_when_waiting_messages_start_the_agent_again() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let child = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go" }),
            )
            .await,
        );
        let (db, inbox, stops) = (&fixture.pool, &tools.inbox, &tools.stops);

        // Asked something mid-run, the child fails before its run commits:
        // the question waits, and will start it again.
        let asked = call(&tools, SEND, json!({ "id": child, "message": "status?" })).await;
        assert!(!asked.is_error, "{}", asked.content);
        let failed = ran(&fixture, child, "").await;
        launcher.end(child);
        report(
            db,
            inbox,
            stops,
            child,
            failed,
            Ended::Failed("terminated".into()),
        )
        .await;
        let (to, text) = launcher.launched().last().cloned().unwrap();
        assert_eq!(to, parent);
        assert_eq!(
            text,
            "The agent failed: terminated\nIt has messages it had not finished with, so it is \
             starting again on them and will report again when it ends.\nIt said nothing."
        );

        // Nothing waiting, nothing said of it.
        let mut conn = db.get().await.unwrap();
        inbox::clear(&mut conn, child).await.unwrap();
        drop(conn);
        launcher.end(parent);
        let finished = ran(&fixture, child, "done").await;
        report(db, inbox, stops, child, finished, Ended::Finished).await;
        assert_eq!(
            launcher.launched().last().unwrap().1,
            "The agent finished.\nIts report:\n\ndone"
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn stopping_a_parent_stops_every_agent_under_it_without_reports() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let task = json!({ "type": "general-purpose", "prompt": "go" });
        let first = spawned(&call(&tools, SPAWN, task.clone()).await);
        let second = spawned(&call(&tools, SPAWN, task).await);
        let mut conn = fixture.pool.get().await.unwrap();
        // Each child has a run under way, as the launcher started it.
        let mut runs = Vec::new();
        for child in [first, second] {
            let thread: Uuid = crate::schema::thread::table
                .filter(crate::schema::thread::session_id.eq(child))
                .select(crate::schema::thread::id)
                .first(&mut conn)
                .await
                .unwrap();
            let run = Uuid::now_v7();
            diesel::sql_query("INSERT INTO run (id, thread_id, created_at) VALUES ($1, $2, 1)")
                .bind::<SqlUuid, _>(run)
                .bind::<SqlUuid, _>(thread)
                .execute(&mut conn)
                .await
                .unwrap();
            runs.push((run, crate::run::open_interrupt(&tools.interrupts, run)));
        }
        drop(conn);
        // One has a message it has not read yet.
        let said = Said {
            text: "and this".into(),
            origin: Origin::agent(parent, None),
        };
        tools.inbox.put(first, said, None).await.unwrap();

        let stopped = stop_under(&fixture.pool, &tools.interrupts, &tools.stops, parent).await;
        assert_eq!(stopped, 2);
        for (run, interrupt) in &runs {
            assert!(interrupt.raised());
            assert!(tools.stops.lock().unwrap().contains(run));
        }
        let mut conn = fixture.pool.get().await.unwrap();
        let waiting: rows::Count =
            diesel::sql_query("SELECT count(*) AS count FROM session_inbox WHERE session_id = $1")
                .bind::<SqlUuid, _>(first)
                .get_result(&mut conn)
                .await
                .unwrap();
        assert_eq!(waiting.count, 0);
        drop(conn);

        // Their runs end, and the parent hears nothing of it.
        for (child, (run, _)) in [first, second].into_iter().zip(&runs) {
            launcher.end(child);
            report(
                &fixture.pool,
                &tools.inbox,
                &tools.stops,
                child,
                *run,
                Ended::Stopped,
            )
            .await;
            assert!(!tools.stops.lock().unwrap().contains(run));
        }
        assert!(
            launcher
                .launched()
                .iter()
                .all(|(session, _)| *session != parent)
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_person_stopping_a_child_lets_its_parent_hear_of_it() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let child = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go" }),
            )
            .await,
        );
        let run = ran(&fixture, child, "partway").await;
        let interrupt = crate::run::open_interrupt(&tools.interrupts, run);
        let mut conn = fixture.pool.get().await.unwrap();
        let stopped = halt(&mut conn, &tools.interrupts, None, child)
            .await
            .unwrap();
        drop(conn);
        assert_eq!(stopped, 1);
        assert!(interrupt.raised());
        assert!(!tools.stops.lock().unwrap().contains(&run));

        launcher.end(child);
        report(
            &fixture.pool,
            &tools.inbox,
            &tools.stops,
            child,
            run,
            Ended::Stopped,
        )
        .await;
        let (to, text) = launcher.launched().last().cloned().unwrap();
        assert_eq!(to, parent);
        assert!(text.starts_with("The agent was stopped"), "{text}");
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_child_run_lost_to_a_restart_is_ended_and_reported_once() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let child = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go" }),
            )
            .await,
        );
        // The process died mid-run: the run never completed.
        let lost = ran(&fixture, child, "half done").await;
        launcher.end(child);
        // The parent's own run, which no agent spawned, is not this
        // recovery's to end.
        let (db, inbox, stops) = (&fixture.pool, &tools.inbox, &tools.stops);
        // Only this test's: other tests' runs are in the same database.
        let recover = || async {
            let mut conn = db.get().await.unwrap();
            let lost = crate::run::lost_children(&mut conn).await.unwrap();
            drop(conn);
            for run in lost {
                if run.session_id == child || run.session_id == parent {
                    crate::run::end_lost(db, inbox, stops, run).await;
                }
            }
        };
        recover().await;

        let mut conn = db.get().await.unwrap();
        let completed: Option<i64> = crate::schema::run::table
            .filter(crate::schema::run::id.eq(lost))
            .select(crate::schema::run::completed_at)
            .first(&mut conn)
            .await
            .unwrap();
        assert!(completed.is_some());
        let parents: Option<i64> = crate::schema::run::table
            .filter(crate::schema::run::id.eq(tools.run))
            .select(crate::schema::run::completed_at)
            .first(&mut conn)
            .await
            .unwrap();
        assert_eq!(parents, None);
        let terminal: String = crate::schema::transcript::table
            .filter(crate::schema::transcript::run_id.eq(lost))
            .order(crate::schema::transcript::seq.desc())
            .select(crate::schema::transcript::kind)
            .first(&mut conn)
            .await
            .unwrap();
        assert_eq!(terminal, crate::run::KIND_RUN_ERROR);
        drop(conn);
        let (to, text) = launcher.launched().last().cloned().unwrap();
        assert_eq!(to, parent);
        assert!(
            text.starts_with("The agent was interrupted by a restart"),
            "{text}"
        );
        assert!(text.ends_with("half done"), "{text}");

        // Once: a second start finds nothing left to recover.
        launcher.end(parent);
        let reports = launcher.launched().len();
        recover().await;
        assert_eq!(launcher.launched().len(), reports);

        // And the child carries on when it is sent a message.
        let sent = call(&tools, SEND, json!({ "id": child, "message": "carry on" })).await;
        assert!(!sent.is_error, "{}", sent.content);
        assert_eq!(
            launcher.launched().last().cloned().unwrap(),
            (child, "carry on".to_owned())
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn spawns_past_the_limits_queue_or_are_refused() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let ceilings = agent_limits::Ceilings {
            runs: 1,
            children: 3,
        };
        tools.limits = ceilings;
        tools.inbox.govern(agent_limits::ProjectRuns::new(
            fixture.pool.clone(),
            ceilings,
        ));
        let task = json!({ "type": "general-purpose", "prompt": "go" });
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(spawned(&call(&tools, SPAWN, task.clone()).await));
        }
        // One runs; the others wait for it.
        assert_eq!(launcher.launched(), [(ids[0], "go".to_owned())]);
        let listed: Value =
            serde_json::from_str(&call(&tools, LIST, json!({})).await.content).unwrap();
        let statuses: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["status"].as_str().unwrap())
            .collect();
        // The fake launcher does not register runs: the one it started
        // reads as idle here.
        assert_eq!(statuses, ["idle", "queued", "queued"]);

        let refused = call(&tools, SPAWN, task.clone()).await;
        assert!(refused.is_error);
        assert!(
            refused.content.contains("agent__send"),
            "{}",
            refused.content
        );

        launcher.end(ids[0]);
        tools.inbox.run_ended(ids[0]).await;
        assert_eq!(launcher.launched()[1].0, ids[1]);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn two_spawns_in_one_turn_answer_before_either_child_finishes() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let (mut tools, launcher) = tools_of(&fixture, parent);
        tools.run = run_of(&fixture).await;
        let mut conn = fixture.pool.get().await.unwrap();
        let reviewer = definitions::New {
            name: "reviewer".into(),
            description: "Reviews a change.".into(),
            prompt: "You review code.".into(),
            ..Default::default()
        };
        definitions::create(&mut conn, fixture.user, fixture.project, reviewer)
            .await
            .unwrap();

        let first = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "reviewer", "prompt": "review the diff", "name": "diff" }),
            )
            .await,
        );
        let second = spawned(
            &call(
                &tools,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "run the tests" }),
            )
            .await,
        );
        // Both are running, each with its prompt, and neither has finished.
        assert_eq!(
            launcher.launched(),
            [
                (first, "review the diff".to_owned()),
                (second, "run the tests".to_owned()),
            ]
        );
        let said = launcher.said.lock().unwrap().clone();
        assert_eq!(said[0].origin, Origin::agent(parent, None));

        let made = children::of(&mut conn, parent).await.unwrap();
        assert_eq!(made.len(), 2);
        assert_eq!(made[0].agent_type.as_deref(), Some("reviewer"));
        assert_eq!(made[0].agent_name.as_deref(), Some("diff"));
        assert_eq!(made[0].spawned_by_run_id, Some(tools.run));
        // A child runs on its parent's model unless its definition says.
        assert_eq!(made[0].model_alias.as_deref(), Some("fast"));
        assert_eq!(made[0].thinking_effort.as_deref(), Some("low"));
        assert_eq!(made[1].agent_type.as_deref(), Some("general-purpose"));

        let unknown = call(&tools, SPAWN, json!({ "type": "planner", "prompt": "x" })).await;
        assert!(unknown.is_error);
        assert!(unknown.content.contains("- reviewer: Reviews a change."));
        assert!(
            call(&tools, SPAWN, json!({ "type": "reviewer" }))
                .await
                .is_error
        );

        let listed = call(&tools, LIST, json!({})).await;
        let listed: Value = serde_json::from_str(&listed.content).unwrap();
        assert_eq!(listed[0]["id"], json!(first));
        assert_eq!(listed[0]["name"], "diff");
        assert_eq!(listed[1]["type"], "general-purpose");
        drop(conn);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_session_reaches_only_the_children_it_spawned() {
        let fixture = Fixture::new(2).await;
        let (mut mine, launcher) = tools_of(&fixture, fixture.sessions[0]);
        mine.run = run_of(&fixture).await;
        let (theirs, _) = tools_of(&fixture, fixture.sessions[1]);
        let child = spawned(
            &call(
                &mine,
                SPAWN,
                json!({ "type": "general-purpose", "prompt": "go" }),
            )
            .await,
        );

        // Another session's child, and no child at all, read the same.
        for id in [child.to_string(), Uuid::now_v7().to_string(), "x".into()] {
            for (name, input) in [
                (SEND, json!({ "id": id, "message": "hi" })),
                (STOP, json!({ "id": id })),
            ] {
                let answer = call(&theirs, name, input).await;
                assert!(answer.is_error, "{name}: {}", answer.content);
                assert!(answer.content.contains("You have started no agent"));
            }
        }
        assert_eq!(
            call(&theirs, LIST, json!({})).await.content,
            "You have started no agents."
        );

        // A finished child is continued: a new run of the same session.
        launcher.end(child);
        let sent = call(
            &mine,
            SEND,
            json!({ "id": child, "message": "and the tests?" }),
        )
        .await;
        assert!(!sent.is_error, "{}", sent.content);
        assert_eq!(launcher.launched()[1], (child, "and the tests?".to_owned()));

        // Stopping drops what it has not read, so nothing starts it again.
        call(&mine, SEND, json!({ "id": child, "message": "one more" })).await;
        let stopped = call(&mine, STOP, json!({ "id": child })).await;
        assert!(!stopped.is_error, "{}", stopped.content);
        launcher.end(child);
        mine.inbox.run_ended(child).await;
        assert_eq!(launcher.launched().len(), 2);

        // A closed child takes no more messages.
        let mut conn = fixture.pool.get().await.unwrap();
        children::close(&mut conn, child, 5).await.unwrap();
        drop(conn);
        assert!(
            call(&mine, SEND, json!({ "id": child, "message": "x" }))
                .await
                .is_error
        );
        fixture.cleanup().await;
    }
}
