//! A user, a project and some sessions in the database `DATABASE_URL`
//! names, for the ignored database tests of daemons and binding storage.

use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use super::BindingKey;
use crate::db::DbPool;

pub(crate) struct Fixture {
    pub pool: DbPool,
    pub user: Uuid,
    pub workspace: Uuid,
    pub project: Uuid,
    pub sessions: Vec<Uuid>,
}

impl Fixture {
    pub async fn new(sessions: usize) -> Fixture {
        let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL (see .env)");
        let pool = crate::db::init_pool(&url).await;
        let mut conn = pool.get().await.unwrap();
        let user = Uuid::now_v7();
        diesel::sql_query("INSERT INTO users (id, identity_id) VALUES ($1, $2)")
            .bind::<SqlUuid, _>(user)
            .bind::<SqlUuid, _>(Uuid::now_v7())
            .execute(&mut conn)
            .await
            .unwrap();
        let workspace = crate::access::personal_workspace(&mut conn, user)
            .await
            .unwrap()
            .id;
        let fixture = Fixture {
            pool: pool.clone(),
            user,
            workspace,
            project: Uuid::nil(),
            sessions: Vec::new(),
        };
        drop(conn);
        let project = fixture.project("daemon").await;
        let mut fixture = Fixture { project, ..fixture };
        for _ in 0..sessions {
            let id = fixture.session(project).await;
            fixture.sessions.push(id);
        }
        fixture
    }

    /// Another project of the same user.
    pub async fn project(&self, name: &str) -> Uuid {
        let mut conn = self.pool.get().await.unwrap();
        let project = Uuid::now_v7();
        diesel::sql_query("INSERT INTO project (id, owner_id, name) VALUES ($1, $2, $3)")
            .bind::<SqlUuid, _>(project)
            .bind::<SqlUuid, _>(self.user)
            .bind::<Text, _>(format!("{name}-{project}"))
            .execute(&mut conn)
            .await
            .unwrap();
        project
    }

    /// A session in `project`, as the owner would start one.
    pub async fn session(&self, project: Uuid) -> Uuid {
        let mut conn = self.pool.get().await.unwrap();
        let id = Uuid::now_v7();
        diesel::insert_into(crate::schema::session::table)
            .values(crate::models::session::NewSession {
                id,
                workspace_id: self.workspace,
                title: None,
                created_at: crate::models::now_epoch(),
                project_id: Some(project),
            })
            .execute(&mut conn)
            .await
            .unwrap();
        id
    }

    pub fn binding(&self, plugin: &str) -> BindingKey {
        BindingKey::new(self.project, plugin)
    }

    /// Deletes the user, and with it every project, session and record.
    pub async fn cleanup(self) {
        let mut conn = self.pool.get().await.unwrap();
        diesel::sql_query(
            "DELETE FROM session WHERE project_id IN (SELECT id FROM project WHERE owner_id = $1)",
        )
        .bind::<SqlUuid, _>(self.user)
        .execute(&mut conn)
        .await
        .unwrap();
        diesel::sql_query("DELETE FROM project WHERE owner_id = $1")
            .bind::<SqlUuid, _>(self.user)
            .execute(&mut conn)
            .await
            .unwrap();
        let _ = diesel::sql_query("DELETE FROM users WHERE id = $1")
            .bind::<SqlUuid, _>(self.user)
            .execute(&mut conn)
            .await;
    }
}
