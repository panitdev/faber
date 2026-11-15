//! Sessions an agent spawned: a child session, and the session and run that
//! started it.
//!
//! A child is an ordinary session in its parent's project and workspace, so
//! it runs as the same owner and whoever may reach the parent may reach it.
//! It is not a top-level session: session listings leave it out, and it is
//! found through its parent. It goes where its parent goes: closing the
//! parent closes it, and deleting the parent deletes it.

use diesel::sql_types::{BigInt, Text, Uuid as SqlUuid};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::models::session::Session;
use crate::models::thread::NewThread;
use crate::schema::{session, thread};

/// Characters of an agent definition's name, and of the name a parent gives
/// a child.
pub const NAME_CHARS: usize = 128;

/// What a parent's agent says of the child it spawns.
#[derive(Clone, Debug)]
pub struct Spawn<'a> {
    /// The parent's run that spawns it.
    pub run: Option<Uuid>,
    /// The name of the agent definition it is spawned as.
    pub agent_type: &'a str,
    /// What the parent calls it.
    pub name: Option<&'a str>,
    /// The model its runs use, and how hard it thinks. The inbox starts a
    /// child's runs on these, so a child without a model never runs.
    pub model_alias: Option<&'a str>,
    pub thinking_effort: Option<&'a str>,
    /// The project's plugins it runs with; `None` is its parent's.
    pub plugins: Option<&'a [String]>,
    /// How many open children the parent may have; `None` is no limit.
    pub limit: Option<u32>,
}

/// Why a child was not created.
#[derive(Debug)]
pub enum SpawnError {
    /// No such parent, or it is outside any project.
    NoParent,
    /// The parent is closed.
    ParentClosed,
    /// The definition's name or the child's name is empty or too long.
    BadName,
    /// The parent already has as many open children as it may.
    TooMany(u32),
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for SpawnError {
    fn from(error: diesel::result::Error) -> Self {
        SpawnError::Db(error)
    }
}

#[derive(diesel::Insertable)]
#[diesel(table_name = session)]
struct NewChild<'a> {
    id: Uuid,
    workspace_id: Uuid,
    title: Option<&'a str>,
    created_at: i64,
    project_id: Option<Uuid>,
    model_alias: Option<&'a str>,
    thinking_effort: Option<&'a str>,
    parent_session_id: Uuid,
    spawned_by_run_id: Option<Uuid>,
    agent_type: &'a str,
    agent_name: Option<&'a str>,
    agent_plugins: Option<&'a [String]>,
}

fn named(name: &str) -> Result<&str, SpawnError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_CHARS {
        return Err(SpawnError::BadName);
    }
    Ok(name)
}

/// Creates a child of `parent` with its root thread, in the parent's project
/// and workspace. Call it in a transaction: a session is never left without
/// a thread.
pub async fn spawn(
    conn: &mut AsyncPgConnection,
    parent: Uuid,
    spawn: Spawn<'_>,
) -> Result<Session, SpawnError> {
    let agent_type = named(spawn.agent_type)?;
    let name = spawn.name.map(named).transpose()?;
    let parent: Session = session::table
        .filter(session::id.eq(parent))
        .select(Session::as_select())
        .first(conn)
        .await
        .optional()?
        .filter(|parent| parent.project_id.is_some())
        .ok_or(SpawnError::NoParent)?;
    if parent.closed_at.is_some() {
        return Err(SpawnError::ParentClosed);
    }
    if let Some(limit) = spawn.limit {
        // Two spawns at once must not both see room for one.
        diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind::<Text, _>(format!("session_children:{}", parent.id))
            .execute(conn)
            .await?;
        let open: i64 = session::table
            .filter(session::parent_session_id.eq(parent.id))
            .filter(session::closed_at.is_null())
            .count()
            .get_result(conn)
            .await?;
        if open >= i64::from(limit) {
            return Err(SpawnError::TooMany(limit));
        }
    }
    let id = Uuid::now_v7();
    let now = crate::models::now_epoch();
    let child: Session = diesel::insert_into(session::table)
        .values(NewChild {
            id,
            workspace_id: parent.workspace_id,
            // Shown wherever a session's title is, until the child earns one.
            title: name,
            created_at: now,
            project_id: parent.project_id,
            model_alias: spawn.model_alias,
            thinking_effort: spawn.thinking_effort,
            parent_session_id: parent.id,
            spawned_by_run_id: spawn.run,
            agent_type,
            agent_name: name,
            agent_plugins: spawn.plugins,
        })
        .returning(Session::as_returning())
        .get_result(conn)
        .await?;
    diesel::insert_into(thread::table)
        .values(NewThread {
            id: Uuid::now_v7(),
            session_id: id,
            parent_id: None,
            forked_at_seq: None,
            created_at: now,
        })
        .execute(conn)
        .await?;
    Ok(child)
}

/// The sessions `parent`'s agent spawned, oldest first.
pub async fn of(
    conn: &mut AsyncPgConnection,
    parent: Uuid,
) -> Result<Vec<Session>, diesel::result::Error> {
    session::table
        .filter(session::parent_session_id.eq(parent))
        .order((session::created_at.asc(), session::id.asc()))
        .select(Session::as_select())
        .load(conn)
        .await
}

// The derive expands the field as `id: id`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    #[derive(diesel::QueryableByName)]
    pub struct Id {
        #[diesel(sql_type = diesel::sql_types::Uuid)]
        pub id: uuid::Uuid,
    }
}

/// Every session under `session`: its children, theirs, and so on.
pub async fn descendants(
    conn: &mut AsyncPgConnection,
    session: Uuid,
) -> Result<Vec<Uuid>, diesel::result::Error> {
    let rows: Vec<rows::Id> = diesel::sql_query(
        "WITH RECURSIVE tree AS ( \
           SELECT id FROM session WHERE parent_session_id = $1 \
           UNION SELECT s.id FROM session s JOIN tree t ON s.parent_session_id = t.id) \
         SELECT id FROM tree",
    )
    .bind::<SqlUuid, _>(session)
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|row| row.id).collect())
}

/// Closes `session` and every session under it that is still open.
pub async fn close(
    conn: &mut AsyncPgConnection,
    session: Uuid,
    now: i64,
) -> Result<(), diesel::result::Error> {
    diesel::sql_query(
        "WITH RECURSIVE tree AS ( \
           SELECT id FROM session WHERE id = $1 \
           UNION SELECT s.id FROM session s JOIN tree t ON s.parent_session_id = t.id) \
         UPDATE session SET closed_at = $2 \
         WHERE id IN (SELECT id FROM tree) AND closed_at IS NULL",
    )
    .bind::<SqlUuid, _>(session)
    .bind::<BigInt, _>(now)
    .execute(conn)
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::daemon::fixture::Fixture;

    fn as_a(agent_type: &str) -> Spawn<'_> {
        Spawn {
            run: None,
            agent_type,
            name: Some("checks the build"),
            model_alias: Some("fast"),
            thinking_effort: None,
            plugins: None,
            limit: None,
        }
    }

    async fn top_level(fixture: &Fixture) -> Vec<Uuid> {
        let mut conn = fixture.pool.get().await.unwrap();
        session::table
            .filter(session::project_id.eq(fixture.project))
            .filter(session::parent_session_id.is_null())
            .select(session::id)
            .load(&mut conn)
            .await
            .unwrap()
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_child_is_its_parent_s_and_not_a_top_level_session() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let mut conn = fixture.pool.get().await.unwrap();

        let child = spawn(&mut conn, parent, as_a("reviewer")).await.unwrap();
        assert_eq!(child.parent_session_id, Some(parent));
        assert_eq!(child.project_id, Some(fixture.project));
        assert_eq!(child.workspace_id, fixture.workspace);
        assert_eq!(child.agent_type.as_deref(), Some("reviewer"));
        assert_eq!(child.agent_name.as_deref(), Some("checks the build"));
        assert_eq!(child.model_alias.as_deref(), Some("fast"));
        let grandchild = spawn(&mut conn, child.id, as_a("general-purpose"))
            .await
            .unwrap();

        let listed = |rows: Vec<Session>| rows.iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(listed(of(&mut conn, parent).await.unwrap()), [child.id]);
        assert_eq!(
            listed(of(&mut conn, child.id).await.unwrap()),
            [grandchild.id]
        );
        assert_eq!(top_level(&fixture).await, [parent]);

        // Whoever may reach the parent may reach the child, and nobody else.
        let owner = crate::access::authorize_session(&mut conn, fixture.user, child.id).await;
        assert_eq!(owner.unwrap().id, child.id);
        let stranger = Fixture::new(0).await;
        assert!(
            crate::access::authorize_session(&mut conn, stranger.user, child.id)
                .await
                .is_err()
        );
        stranger.cleanup().await;

        assert!(matches!(
            spawn(&mut conn, Uuid::now_v7(), as_a("reviewer")).await,
            Err(SpawnError::NoParent)
        ));
        assert!(matches!(
            spawn(&mut conn, parent, as_a(" ")).await,
            Err(SpawnError::BadName)
        ));
        drop(conn);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_parent_has_no_more_open_children_than_its_limit() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let mut conn = fixture.pool.get().await.unwrap();
        let capped = || Spawn {
            limit: Some(2),
            ..as_a("reviewer")
        };
        let first = spawn(&mut conn, parent, capped()).await.unwrap();
        spawn(&mut conn, parent, capped()).await.unwrap();
        assert!(matches!(
            spawn(&mut conn, parent, capped()).await,
            Err(SpawnError::TooMany(2))
        ));
        // A closed child no longer counts.
        close(&mut conn, first.id, 1).await.unwrap();
        spawn(&mut conn, parent, capped()).await.unwrap();
        drop(conn);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_child_cannot_name_a_parent_in_another_project() {
        let fixture = Fixture::new(1).await;
        let parent = fixture.sessions[0];
        let elsewhere = fixture.project("other").await;
        let mut conn = fixture.pool.get().await.unwrap();
        let insert = |project: Option<Uuid>| {
            diesel::insert_into(session::table).values(NewChild {
                id: Uuid::now_v7(),
                workspace_id: fixture.workspace,
                title: None,
                created_at: 1,
                project_id: project,
                model_alias: None,
                thinking_effort: None,
                parent_session_id: parent,
                spawned_by_run_id: None,
                agent_type: "reviewer",
                agent_name: None,
                agent_plugins: None,
            })
        };
        assert!(insert(Some(elsewhere)).execute(&mut conn).await.is_err());
        assert!(insert(None).execute(&mut conn).await.is_err());
        assert_eq!(
            insert(Some(fixture.project)).execute(&mut conn).await,
            Ok(1)
        );
        drop(conn);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn children_are_closed_and_deleted_with_their_parent() {
        let fixture = Fixture::new(2).await;
        let [parent, bystander] = fixture.sessions[..] else {
            unreachable!()
        };
        let mut conn = fixture.pool.get().await.unwrap();
        let child = spawn(&mut conn, parent, as_a("reviewer")).await.unwrap().id;
        let grandchild = spawn(&mut conn, child, as_a("reviewer")).await.unwrap().id;
        let mut under = descendants(&mut conn, parent).await.unwrap();
        under.sort();
        assert_eq!(under, [child, grandchild]);

        close(&mut conn, parent, 7).await.unwrap();
        let closed: Vec<(Uuid, Option<i64>)> = session::table
            .filter(session::project_id.eq(fixture.project))
            .order(session::id.asc())
            .select((session::id, session::closed_at))
            .load(&mut conn)
            .await
            .unwrap();
        assert_eq!(
            closed,
            [
                (parent, Some(7)),
                (bystander, None),
                (child, Some(7)),
                (grandchild, Some(7)),
            ]
        );
        // A closed session spawns nothing.
        assert!(matches!(
            spawn(&mut conn, parent, as_a("reviewer")).await,
            Err(SpawnError::ParentClosed)
        ));

        diesel::delete(session::table.filter(session::id.eq(parent)))
            .execute(&mut conn)
            .await
            .unwrap();
        let left: Vec<Uuid> = session::table
            .filter(session::project_id.eq(fixture.project))
            .select(session::id)
            .load(&mut conn)
            .await
            .unwrap();
        assert_eq!(left, [bystander]);
        drop(conn);
        fixture.cleanup().await;
    }
}
