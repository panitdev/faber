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
//! A session reaches only the children it spawned. Children are not granted
//! the tools, so the tree is one level deep.

use std::sync::Arc;

use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use harness::mapping::ToolResult;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::children::{self, Spawn, SpawnError};
use crate::db::DbPool;
use crate::definitions::{self, Definition};
use crate::inbox::{self, Inbox, PutError, Refused, Said};
use crate::models::session::Session;
use crate::run::{InterruptRegistry, RunRegistry};
use crate::said::Origin;

pub const SPAWN: &str = "agent__spawn";
pub const SEND: &str = "agent__send";
pub const STOP: &str = "agent__stop";
pub const LIST: &str = "agent__list";

/// Characters of a prompt or a message one agent sends another.
const MESSAGE_CHARS: usize = 64_000;

/// Characters of the line `agent__list` gives on what a child is doing.
const DOING_CHARS: usize = 160;

/// Added to the head of a session an agent spawned, after its definition's
/// prompt.
pub const SUBAGENT_PROMPT: &str = "You are a subagent: another agent working in this project \
    started you to do the task in its first message, and messages with `source=\"agent\"` are \
    from it. Nobody else reads what you write. When you finish, your final message is delivered \
    to that agent as your report, so make it brief and complete: what you found or did, and \
    anything it needs to know to carry on.";

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
                 Spawn several in one turn to have them work at the same time.\n\n\
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
                working reads the message after its current step."
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
            description: "Lists the agents you started: each one's id, type, name, whether it is \
                running, and the last thing it said or did."
                .to_owned(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
    ]
}

/// The tools of one run of a session.
pub struct AgentTools {
    pub db: DbPool,
    pub inbox: Arc<Inbox>,
    pub runs: RunRegistry,
    pub interrupts: InterruptRegistry,
    /// The session whose agent calls them.
    pub session: Uuid,
    pub project: Uuid,
    /// The project's owner, whose models the children run on.
    pub owner: Uuid,
    /// The run that calls them.
    pub run: Uuid,
    /// What this run runs on: a child's, unless its definition says otherwise.
    pub model_alias: String,
    pub thinking_effort: Option<String>,
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
fn doing(payload: &Value) -> Option<String> {
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

impl AgentTools {
    /// Answers a call of one of the tools, or `None` for any other name.
    pub async fn invoke(&self, name: &str, input: &Value) -> Option<ToolResult> {
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
        let spawn = Spawn {
            run: Some(self.run),
            agent_type: &definition.name,
            name,
            model_alias: Some(&model),
            thinking_effort: self.thinking_effort.as_deref(),
            plugins: definition.plugins.as_deref(),
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
                SpawnError::ParentClosed | SpawnError::NoParent => {
                    refused("This session is closed and cannot start agents.")
                }
                SpawnError::Db(error) => unavailable(error),
            })?;
        drop(conn);
        self.put(child.id, prompt).await?;
        Ok(ok(
            json!({ "id": child.id, "type": definition.name, "status": "started" }).to_string(),
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

    async fn stop(&self, input: &Value) -> Result<ToolResult, ToolResult> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let child = self.child(&mut conn, input).await?;
        // What it has not read goes first, so the end of the run being
        // stopped finds nothing to start another with.
        conn.transaction::<(), diesel::result::Error, _>(|conn| {
            async move {
                inbox::lock(conn, child.id).await?;
                inbox::clear(conn, child.id).await
            }
            .scope_boxed()
        })
        .await
        .map_err(unavailable)?;
        let live: Vec<rows::RunId> = diesel::sql_query(
            "SELECT r.id FROM run r JOIN thread t ON t.id = r.thread_id \
             WHERE t.session_id = $1 AND r.completed_at IS NULL",
        )
        .bind::<SqlUuid, _>(child.id)
        .load(&mut conn)
        .await
        .map_err(unavailable)?;
        let stopped = live
            .iter()
            .filter(|run| crate::run::raise_interrupt(&self.interrupts, run.id))
            .count();
        Ok(ok(if stopped > 0 {
            "Stopping. It keeps what it has done so far."
        } else {
            "It was not running."
        }))
    }

    async fn list(&self) -> Result<ToolResult, ToolResult> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        let spawned = children::of(&mut conn, self.session)
            .await
            .map_err(unavailable)?;
        let mut agents = Vec::with_capacity(spawned.len());
        for child in spawned {
            let status = if child.closed_at.is_some() {
                "closed"
            } else if crate::run::is_running(&self.runs, child.id) {
                "running"
            } else {
                let waiting: rows::Count = diesel::sql_query(
                    "SELECT count(*) AS count FROM session_inbox WHERE session_id = $1",
                )
                .bind::<SqlUuid, _>(child.id)
                .get_result(&mut conn)
                .await
                .map_err(unavailable)?;
                // Messages wait on one that is not running when it cannot
                // start: no run slot yet, or no model.
                if waiting.count > 0 { "waiting" } else { "idle" }
            };
            let last: Option<rows::Payload> = diesel::sql_query(
                "SELECT x.payload FROM transcript x \
                 JOIN run r ON r.id = x.run_id JOIN thread t ON t.id = r.thread_id \
                 WHERE t.session_id = $1 AND x.kind = $2 \
                 ORDER BY r.created_at DESC, r.id DESC, x.seq DESC LIMIT 1",
            )
            .bind::<SqlUuid, _>(child.id)
            .bind::<Text, _>(crate::compact::KIND_MESSAGE)
            .get_result(&mut conn)
            .await
            .optional()
            .map_err(unavailable)?;
            let mut agent = json!({
                "id": child.id,
                "type": child.agent_type,
                "status": status,
            });
            if let Some(name) = &child.agent_name {
                agent["name"] = json!(name);
            }
            if let Some(line) = last.and_then(|row| doing(&row.payload)) {
                agent["last"] = json!(line);
            }
            agents.push(agent);
        }
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
            session,
            project: fixture.project,
            owner: fixture.user,
            run: Uuid::now_v7(),
            model_alias: "fast".into(),
            thinking_effort: Some("low".into()),
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
