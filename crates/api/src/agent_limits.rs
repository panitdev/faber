//! How much the agents a project's sessions spawn may run.
//!
//! Every run of a spawned session spends the project owner's credentials, and
//! one turn can spawn many. Two limits bound that: how many runs of spawned
//! sessions a project has under way at once, and how many open sessions one
//! session may have spawned. A run past the first waits its turn in its
//! session's inbox, so a wide fan-out drains through the limit; a spawn past
//! the second is refused. The owner sets each on the project, held to the
//! operator's ceiling; unset is the ceiling.

use std::sync::Arc;

use async_trait::async_trait;
use diesel::OptionalExtension;
use diesel::sql_types::Uuid as SqlUuid;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::db::DbPool;
use crate::inbox::{Admission, Launcher, Places};

/// The operator's ceilings on every project's agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ceilings {
    /// Runs of one project's spawned sessions at once.
    pub runs: u32,
    /// Open sessions one session may have spawned.
    pub children: u32,
}

impl Default for Ceilings {
    fn default() -> Self {
        Ceilings {
            runs: 4,
            children: 8,
        }
    }
}

impl Ceilings {
    /// `FABER_AGENT_MAX_RUNS` and `FABER_AGENT_MAX_CHILDREN`, each a positive
    /// whole number; unset or blank is the default. A value that does not
    /// parse is refused.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Ceilings, String> {
        let read = |name: &str, default: u32| -> Result<u32, String> {
            match var(name).as_deref().map(str::trim) {
                None | Some("") => Ok(default),
                Some(text) => match text.parse::<u32>() {
                    Ok(value) if value > 0 => Ok(value),
                    _ => Err(format!(
                        "{name} must be a positive whole number, not `{text}`"
                    )),
                },
            }
        };
        let default = Ceilings::default();
        Ok(Ceilings {
            runs: read("FABER_AGENT_MAX_RUNS", default.runs)?,
            children: read("FABER_AGENT_MAX_CHILDREN", default.children)?,
        })
    }

    pub fn from_env() -> Ceilings {
        Ceilings::from_vars(|name| std::env::var(name).ok())
            .unwrap_or_else(|message| panic!("{message}"))
    }
}

/// What the owner set, held to `ceiling`; unset is the ceiling.
pub fn held(set: Option<i32>, ceiling: u32) -> u32 {
    set.and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .map_or(ceiling, |value| value.min(ceiling))
}

// The derive expands each field as `field: field`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::sql_types::{Integer, Nullable, Uuid as SqlUuid};
    use uuid::Uuid;

    /// A spawned session's project, and the owner's limit on its runs.
    #[derive(diesel::QueryableByName)]
    pub struct Spawned {
        #[diesel(sql_type = SqlUuid)]
        pub project_id: Uuid,
        #[diesel(sql_type = Nullable<Integer>)]
        pub max_agent_runs: Option<i32>,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Limit {
        #[diesel(sql_type = Nullable<Integer>)]
        pub max_session_agents: Option<i32>,
    }

    #[derive(diesel::QueryableByName)]
    pub struct SessionId {
        #[diesel(sql_type = SqlUuid)]
        pub session_id: Uuid,
    }
}

fn logged(error: impl std::fmt::Display) {
    tracing::warn!(%error, "agent limit call failed");
}

/// How many open sessions a session of `project` may have spawned.
pub async fn children_cap(
    conn: &mut AsyncPgConnection,
    project: Uuid,
    ceilings: Ceilings,
) -> Result<u32, diesel::result::Error> {
    let found: Option<rows::Limit> =
        diesel::sql_query("SELECT max_session_agents FROM project WHERE id = $1")
            .bind::<SqlUuid, _>(project)
            .get_result(conn)
            .await
            .optional()?;
    Ok(held(
        found.and_then(|row| row.max_session_agents),
        ceilings.children,
    ))
}

/// Holds a spawned session's run back while its project already runs as
/// many as it may. Sessions nobody spawned are never held back here.
pub struct ProjectRuns {
    db: DbPool,
    ceilings: Ceilings,
    running: Places<Uuid>,
}

impl ProjectRuns {
    pub fn new(db: DbPool, ceilings: Ceilings) -> Arc<Self> {
        Arc::new(ProjectRuns {
            db,
            ceilings,
            running: Places::default(),
        })
    }

    /// `project`'s spawned sessions with messages waiting, oldest first.
    async fn waiting_of(&self, project: Uuid) -> Vec<Uuid> {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return Vec::new();
        };
        let rows: Vec<rows::SessionId> = match diesel::sql_query(
            "SELECT i.session_id FROM session_inbox i JOIN session s ON s.id = i.session_id \
             WHERE s.project_id = $1 AND s.parent_session_id IS NOT NULL \
             GROUP BY i.session_id ORDER BY min(i.id)",
        )
        .bind::<SqlUuid, _>(project)
        .load(&mut conn)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                logged(error);
                return Vec::new();
            }
        };
        rows.into_iter().map(|row| row.session_id).collect()
    }
}

#[async_trait]
impl Admission for ProjectRuns {
    async fn admit(&self, session: Uuid, launcher: &dyn Launcher) -> bool {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return false;
        };
        let found: Option<rows::Spawned> = match diesel::sql_query(
            "SELECT s.project_id, p.max_agent_runs FROM session s \
             JOIN project p ON p.id = s.project_id \
             WHERE s.id = $1 AND s.parent_session_id IS NOT NULL",
        )
        .bind::<SqlUuid, _>(session)
        .get_result(&mut conn)
        .await
        .optional()
        {
            Ok(found) => found,
            Err(error) => {
                logged(error);
                return false;
            }
        };
        drop(conn);
        let Some(spawned) = found else {
            return true;
        };
        let cap = held(spawned.max_agent_runs, self.ceilings.runs);
        self.running
            .take(launcher, &spawned.project_id, session, Some(cap))
    }

    fn started(&self, session: Uuid) {
        self.running.started(session);
    }

    fn withdraw(&self, session: Uuid) {
        self.running.free(session);
    }

    async fn ended(&self, session: Uuid) -> Vec<Uuid> {
        match self.running.free(session) {
            Some(project) => self.waiting_of(project).await,
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::children::{self, Spawn};
    use crate::inbox::testing::FakeLauncher;
    use crate::inbox::{Inbox, Said};
    use crate::plugins::daemon::fixture::Fixture;
    use crate::said::Origin;
    use diesel_async::AsyncConnection;
    use diesel_async::scoped_futures::ScopedFutureExt;

    #[test]
    fn ceilings_come_from_their_variables_and_hold_the_owner() {
        assert_eq!(Ceilings::from_vars(|_| None).unwrap(), Ceilings::default());
        let set = Ceilings::from_vars(|name| match name {
            "FABER_AGENT_MAX_RUNS" => Some("2".into()),
            "FABER_AGENT_MAX_CHILDREN" => Some(" ".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            set,
            Ceilings {
                runs: 2,
                children: Ceilings::default().children
            }
        );
        assert!(Ceilings::from_vars(|_| Some("0".into())).is_err());
        assert_eq!(held(None, 4), 4);
        assert_eq!(held(Some(2), 4), 2);
        assert_eq!(held(Some(9), 4), 4);
        assert_eq!(held(Some(0), 4), 4);
    }

    async fn spawned(fixture: &Fixture, parent: Uuid, count: usize) -> Vec<Uuid> {
        let mut conn = fixture.pool.get().await.unwrap();
        let mut ids = Vec::new();
        for _ in 0..count {
            let child = conn
                .transaction::<_, children::SpawnError, _>(|conn| {
                    async move {
                        children::spawn(
                            conn,
                            parent,
                            Spawn {
                                run: None,
                                agent_type: "general-purpose",
                                name: None,
                                model_alias: None,
                                thinking_effort: None,
                                plugins: None,
                                limit: None,
                            },
                        )
                        .await
                    }
                    .scope_boxed()
                })
                .await
                .unwrap();
            ids.push(child.id);
        }
        ids
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_fan_out_drains_through_the_project_s_limit_oldest_first() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let inbox = Inbox::new(fixture.pool.clone());
        let launcher = Arc::new(FakeLauncher::default());
        inbox.attach(launcher.clone());
        inbox.govern(ProjectRuns::new(
            fixture.pool.clone(),
            Ceilings {
                runs: 2,
                children: 8,
            },
        ));
        let children = spawned(&fixture, parent, 5).await;
        for (n, child) in children.iter().enumerate() {
            let said = Said {
                text: format!("task {n}"),
                origin: Origin::agent(parent, None),
            };
            inbox.put(*child, said, None).await.unwrap();
        }
        let started = |launcher: &FakeLauncher| {
            launcher
                .launched()
                .into_iter()
                .map(|(session, _)| session)
                .collect::<Vec<_>>()
        };
        assert_eq!(started(&launcher), children[..2]);

        // The parent is no spawned session: nothing holds it back.
        let said = Said {
            text: "go on".into(),
            origin: Origin::faber(Uuid::nil()),
        };
        inbox.put(parent, said, None).await.unwrap();
        assert_eq!(started(&launcher).last(), Some(&parent));
        launcher.end(parent);
        inbox.run_ended(parent).await;

        // Each run that ends lets the oldest waiting one start.
        for (ending, next) in [(1, 2), (0, 3), (2, 4)] {
            launcher.end(children[ending]);
            inbox.run_ended(children[ending]).await;
            assert_eq!(started(&launcher).last(), Some(&children[next]));
        }
        for child in &children[3..] {
            launcher.end(*child);
            inbox.run_ended(*child).await;
        }
        let ran: Vec<Uuid> = started(&launcher)
            .into_iter()
            .filter(|session| *session != parent)
            .collect();
        assert_eq!(
            ran,
            [
                children[0],
                children[1],
                children[2],
                children[3],
                children[4]
            ]
        );
        fixture.cleanup().await;
    }
}
