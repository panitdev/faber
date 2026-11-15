//! What each agent a session spawned is doing: the state a client shows for
//! a parent's children, and that `agent__list` tells the parent's agent.
//!
//! Built from the database and the runs under way in this process, so it is
//! the same whoever asks and however often.

use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::run::RunRegistry;

/// Where an agent is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Messages wait on it and no run has started for them yet: its turn
    /// under a limit has not come.
    Queued,
    Running,
    /// Its last run finished, or it has not run and nothing waits on it.
    Idle,
    /// Its last run ended in an error.
    Failed,
    /// Its last run was stopped.
    Stopped,
}

/// The tokens an agent's runs used, and what they cost where the provider
/// said. A client prices what it did not say from the model's prices.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    /// The sum of the costs providers reported; `None` when none did.
    pub cost: Option<f64>,
    /// Whether every call reported its cost, so `cost` is the whole of it.
    pub costed: bool,
}

/// One agent a session spawned.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Agent {
    pub id: Uuid,
    /// What its parent calls it.
    pub name: Option<String>,
    /// The agent definition it was spawned as.
    #[serde(rename = "type")]
    pub agent_type: Option<String>,
    /// The model alias its runs use.
    pub model: Option<String>,
    pub status: Status,
    /// Closed with its parent: it runs no more.
    pub closed: bool,
    /// Its latest tool call, or the start of its latest text, on one line.
    pub activity: Option<String>,
    pub usage: Usage,
    pub created_at: i64,
    /// When its first run started.
    pub started_at: Option<i64>,
    /// When its last run ended, unless one is under way.
    pub ended_at: Option<i64>,
}

// The derive expands each field as `field: field`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::sql_types::{BigInt, Bool, Double, Jsonb, Nullable, Text, Uuid as SqlUuid};
    use uuid::Uuid;

    #[derive(diesel::QueryableByName)]
    pub struct Child {
        #[diesel(sql_type = SqlUuid)]
        pub id: Uuid,
        #[diesel(sql_type = Nullable<Text>)]
        pub agent_name: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        pub agent_type: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        pub model_alias: Option<String>,
        #[diesel(sql_type = BigInt)]
        pub created_at: i64,
        #[diesel(sql_type = Nullable<BigInt>)]
        pub closed_at: Option<i64>,
        #[diesel(sql_type = BigInt)]
        pub waiting: i64,
        #[diesel(sql_type = Nullable<BigInt>)]
        pub first_started: Option<i64>,
        #[diesel(sql_type = Nullable<BigInt>)]
        pub last_completed: Option<i64>,
        #[diesel(sql_type = Nullable<Text>)]
        pub last_terminal: Option<String>,
        #[diesel(sql_type = BigInt)]
        pub input_tokens: i64,
        #[diesel(sql_type = BigInt)]
        pub output_tokens: i64,
        #[diesel(sql_type = BigInt)]
        pub cache_read_tokens: i64,
        #[diesel(sql_type = BigInt)]
        pub cache_write_tokens: i64,
        #[diesel(sql_type = BigInt)]
        pub reasoning_tokens: i64,
        #[diesel(sql_type = Nullable<Double>)]
        pub cost: Option<f64>,
        #[diesel(sql_type = Bool)]
        pub costed: bool,
        #[diesel(sql_type = Nullable<Jsonb>)]
        pub last_said: Option<serde_json::Value>,
    }
}

/// The agents `parent` spawned, oldest first.
pub async fn of(
    conn: &mut AsyncPgConnection,
    runs: &RunRegistry,
    parent: Uuid,
) -> Result<Vec<Agent>, diesel::result::Error> {
    let children: Vec<rows::Child> = diesel::sql_query(
        "SELECT s.id, s.agent_name, s.agent_type, s.model_alias, s.created_at, s.closed_at, \
                (SELECT count(*) FROM session_inbox i WHERE i.session_id = s.id) AS waiting, \
                first.started AS first_started, last.completed_at AS last_completed, \
                last.terminal AS last_terminal, \
                used.input_tokens, used.output_tokens, used.cache_read_tokens, \
                used.cache_write_tokens, used.reasoning_tokens, used.cost, used.costed, \
                said.payload AS last_said \
         FROM session s \
         LEFT JOIN LATERAL ( \
           SELECT min(r.created_at) AS started FROM run r \
           JOIN thread t ON t.id = r.thread_id WHERE t.session_id = s.id) first ON true \
         LEFT JOIN LATERAL ( \
           SELECT r.completed_at, \
                  (SELECT x.kind FROM transcript x WHERE x.run_id = r.id \
                   AND x.kind IN ($2, $3, $4) ORDER BY x.seq DESC LIMIT 1) AS terminal \
           FROM run r JOIN thread t ON t.id = r.thread_id WHERE t.session_id = s.id \
           ORDER BY r.created_at DESC, r.id DESC LIMIT 1) last ON true \
         LEFT JOIN LATERAL ( \
           SELECT coalesce(sum((e.usage->>'input_tokens')::bigint), 0)::bigint AS input_tokens, \
                  coalesce(sum((e.usage->>'output_tokens')::bigint), 0)::bigint AS output_tokens, \
                  coalesce(sum((e.usage->>'cache_read_tokens')::bigint), 0)::bigint \
                    AS cache_read_tokens, \
                  coalesce(sum((e.usage->>'cache_write_tokens')::bigint), 0)::bigint \
                    AS cache_write_tokens, \
                  coalesce(sum((e.usage->>'reasoning_tokens')::bigint), 0)::bigint \
                    AS reasoning_tokens, \
                  sum((e.usage->>'cost')::float8) AS cost, \
                  coalesce(bool_and(e.usage->'cost' IS NOT NULL \
                    AND jsonb_typeof(e.usage->'cost') = 'number'), false) AS costed \
           FROM exchange e JOIN run r ON r.id = e.run_id JOIN thread t ON t.id = r.thread_id \
           WHERE t.session_id = s.id AND e.usage IS NOT NULL) used ON true \
         LEFT JOIN LATERAL ( \
           SELECT x.payload FROM transcript x \
           JOIN run r ON r.id = x.run_id JOIN thread t ON t.id = r.thread_id \
           WHERE t.session_id = s.id AND x.kind = $5 AND x.payload->>'role' = 'assistant' \
           ORDER BY r.created_at DESC, r.id DESC, x.seq DESC LIMIT 1) said ON true \
         WHERE s.parent_session_id = $1 \
         ORDER BY s.created_at, s.id",
    )
    .bind::<SqlUuid, _>(parent)
    .bind::<Text, _>(crate::run::KIND_RUN_END)
    .bind::<Text, _>(crate::run::KIND_RUN_ERROR)
    .bind::<Text, _>(crate::run::KIND_RUN_INTERRUPTED)
    .bind::<Text, _>(crate::compact::KIND_MESSAGE)
    .load(conn)
    .await?;
    Ok(children
        .into_iter()
        .map(|child| {
            let running = crate::run::is_running(runs, child.id);
            let status = if running {
                Status::Running
            } else if child.waiting > 0 && child.closed_at.is_none() {
                Status::Queued
            } else {
                match child.last_terminal.as_deref() {
                    Some(crate::run::KIND_RUN_ERROR) => Status::Failed,
                    Some(crate::run::KIND_RUN_INTERRUPTED) => Status::Stopped,
                    _ => Status::Idle,
                }
            };
            Agent {
                id: child.id,
                name: child.agent_name,
                agent_type: child.agent_type,
                model: child.model_alias,
                status,
                closed: child.closed_at.is_some(),
                activity: child.last_said.as_ref().and_then(crate::subagents::doing),
                usage: Usage {
                    input_tokens: child.input_tokens,
                    output_tokens: child.output_tokens,
                    cache_read_tokens: child.cache_read_tokens,
                    cache_write_tokens: child.cache_write_tokens,
                    reasoning_tokens: child.reasoning_tokens,
                    cost: child.cost,
                    costed: child.costed,
                },
                created_at: child.created_at,
                started_at: child.first_started,
                ended_at: if running { None } else { child.last_completed },
            }
        })
        .collect())
}

/// The agents of `now` that are new or changed since `before`, in `now`'s
/// order: what a feed of them sends next.
pub fn changed<'a>(before: &[Agent], now: &'a [Agent]) -> Vec<&'a Agent> {
    now.iter()
        .filter(|agent| !before.iter().any(|seen| seen == *agent))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::{ExpressionMethods, QueryDsl};

    fn agent(id: u128, status: Status) -> Agent {
        Agent {
            id: Uuid::from_u128(id),
            name: None,
            agent_type: Some("general-purpose".into()),
            model: None,
            status,
            closed: false,
            activity: None,
            usage: Usage::default(),
            created_at: 1,
            started_at: None,
            ended_at: None,
        }
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn each_child_reads_with_its_status_activity_and_usage() {
        use crate::children::{self, Spawn};
        use diesel::sql_types::{BigInt, Bytea, Jsonb};
        let fixture = crate::plugins::daemon::fixture::Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let mut conn = fixture.pool.get().await.unwrap();
        let mut ids = Vec::new();
        for name in ["failed", "queued", "fresh"] {
            let spawn = Spawn {
                run: None,
                agent_type: "general-purpose",
                name: Some(name),
                model_alias: Some("fast"),
                thinking_effort: None,
                plugins: None,
                limit: None,
            };
            ids.push(children::spawn(&mut conn, parent, spawn).await.unwrap().id);
        }
        let thread: Uuid = crate::schema::thread::table
            .filter(crate::schema::thread::session_id.eq(ids[0]))
            .select(crate::schema::thread::id)
            .first(&mut conn)
            .await
            .unwrap();
        let run = Uuid::now_v7();
        diesel::sql_query(
            "INSERT INTO run (id, thread_id, created_at, completed_at) VALUES ($1, $2, 10, 20)",
        )
        .bind::<SqlUuid, _>(run)
        .bind::<SqlUuid, _>(thread)
        .execute(&mut conn)
        .await
        .unwrap();
        for (seq, kind, payload) in [
            (
                1,
                crate::compact::KIND_MESSAGE,
                serde_json::json!({ "role": "assistant", "content": [
                    { "type": "text", "text": "Looking." },
                    { "type": "tool_use", "id": "t1", "name": "read_file", "input": {} },
                ]}),
            ),
            (
                2,
                crate::compact::KIND_MESSAGE,
                serde_json::json!({ "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": "x" },
                ]}),
            ),
            (
                3,
                crate::run::KIND_RUN_ERROR,
                serde_json::json!({ "message": "no credit" }),
            ),
        ] {
            diesel::sql_query(
                "INSERT INTO transcript (id, run_id, seq, kind, payload, created_at) \
                 VALUES ($1, $2, $3, $4, $5, 1)",
            )
            .bind::<SqlUuid, _>(Uuid::now_v7())
            .bind::<SqlUuid, _>(run)
            .bind::<BigInt, _>(seq)
            .bind::<Text, _>(kind)
            .bind::<Jsonb, _>(payload)
            .execute(&mut conn)
            .await
            .unwrap();
        }
        let digest = Uuid::now_v7().as_bytes().to_vec();
        diesel::sql_query(
            "INSERT INTO blob (digest, data, byte_length, created_at) VALUES ($1, '', 0, 1)",
        )
        .bind::<Bytea, _>(&digest)
        .execute(&mut conn)
        .await
        .unwrap();
        for cost in [Some(0.25), None] {
            diesel::sql_query(
                "INSERT INTO exchange (id, run_id, request_blob_digest, usage, \
                   expected_cache_tokens, started_at) VALUES ($1, $2, $3, $4, 0, 10)",
            )
            .bind::<SqlUuid, _>(Uuid::now_v7())
            .bind::<SqlUuid, _>(run)
            .bind::<Bytea, _>(&digest)
            .bind::<Jsonb, _>(serde_json::json!({
                "input_tokens": 100, "output_tokens": 10, "reasoning_tokens": 0,
                "cache_read_tokens": 50, "cache_write_tokens": 0, "cost": cost,
            }))
            .execute(&mut conn)
            .await
            .unwrap();
        }
        diesel::sql_query(
            "INSERT INTO session_inbox (session_id, content, origin) VALUES ($1, 'go', '{}')",
        )
        .bind::<SqlUuid, _>(ids[1])
        .execute(&mut conn)
        .await
        .unwrap();

        let agents = of(&mut conn, &Default::default(), parent).await.unwrap();
        let statuses: Vec<Status> = agents.iter().map(|agent| agent.status).collect();
        assert_eq!(statuses, [Status::Failed, Status::Queued, Status::Idle]);
        let failed = &agents[0];
        assert_eq!(failed.name.as_deref(), Some("failed"));
        assert_eq!(failed.model.as_deref(), Some("fast"));
        assert_eq!(failed.activity.as_deref(), Some("calling read_file"));
        assert_eq!((failed.started_at, failed.ended_at), (Some(10), Some(20)));
        assert_eq!(failed.usage.input_tokens, 200);
        assert_eq!(failed.usage.cache_read_tokens, 100);
        assert_eq!(failed.usage.cost, Some(0.25));
        assert!(!failed.usage.costed);
        assert_eq!(agents[2].usage, Usage::default());
        assert_eq!(agents[2].started_at, None);

        drop(conn);
        // The exchanges go with the session, by cascade; then their blob.
        let pool = fixture.pool.clone();
        fixture.cleanup().await;
        let mut conn = pool.get().await.unwrap();
        diesel::sql_query("DELETE FROM blob WHERE digest = $1")
            .bind::<Bytea, _>(&digest)
            .execute(&mut conn)
            .await
            .unwrap();
    }

    #[test]
    fn a_feed_sends_only_what_is_new_or_changed() {
        let before = [agent(1, Status::Running), agent(2, Status::Queued)];
        let mut now = before.to_vec();
        assert!(changed(&before, &now).is_empty());
        now[1].status = Status::Running;
        now.push(agent(3, Status::Queued));
        let sent: Vec<Uuid> = changed(&before, &now).iter().map(|a| a.id).collect();
        assert_eq!(sent, [Uuid::from_u128(2), Uuid::from_u128(3)]);
        // Opened afresh, it sends every one.
        assert_eq!(changed(&[], &now).len(), 3);
    }

    #[test]
    fn an_agent_reads_as_the_client_expects() {
        let mut one = agent(1, Status::Stopped);
        one.usage.cost = Some(0.5);
        let json = serde_json::to_value(&one).unwrap();
        assert_eq!(json["status"], "stopped");
        assert_eq!(json["type"], "general-purpose");
        assert_eq!(json["usage"]["cost"], 0.5);
        assert_eq!(json["ended_at"], serde_json::Value::Null);
    }
}
