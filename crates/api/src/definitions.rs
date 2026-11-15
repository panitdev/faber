//! Agent definitions: the kinds of agent a session of a project may spawn.
//!
//! A definition is a name, a description the spawning model reads to choose
//! it, a prompt for the spawned session, and optionally a model and a set of
//! the project's plugins; either left unset is the parent's. They are the
//! roles of a team, and how one step gets its own model.
//!
//! Every project has [`GENERAL_PURPOSE`], which is built in, inherits
//! everything, and cannot be changed or deleted. A session records the name
//! it was spawned as and nothing else of the definition, so changing or
//! deleting one leaves the sessions already spawned as they were.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::error::{ApiResult, AppError};
use crate::schema::{models, project_agent, project_binding};

/// The definition every project has.
pub const GENERAL_PURPOSE: &str = "general-purpose";

const GENERAL_PURPOSE_DESCRIPTION: &str = "An agent like you, with your model and your tools. \
    Use it for any task that no other agent is described as being for.";

/// Characters of a name: lower-case letters, digits and hyphens.
pub const NAME_CHARS: usize = 64;
pub const DESCRIPTION_CHARS: usize = 1000;
pub const PROMPT_CHARS: usize = 32_000;
/// Definitions one project holds. Every one is described to the model in
/// each run, so the list is kept short.
pub const PER_PROJECT: i64 = 32;

/// One kind of agent a project's sessions may spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub description: String,
    pub prompt: String,
    /// The owner's model alias its sessions run on; `None` is the parent's.
    pub model_alias: Option<String>,
    /// The project's plugins its sessions get; `None` is the parent's.
    pub plugins: Option<Vec<String>>,
    /// When it was last changed; `None` for the built-in.
    pub updated_at: Option<DateTime<Utc>>,
}

impl Definition {
    pub fn builtin(&self) -> bool {
        self.updated_at.is_none()
    }

    fn general_purpose() -> Self {
        Definition {
            name: GENERAL_PURPOSE.to_owned(),
            description: GENERAL_PURPOSE_DESCRIPTION.to_owned(),
            prompt: String::new(),
            model_alias: None,
            plugins: None,
            updated_at: None,
        }
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = project_agent)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct Row {
    name: String,
    description: String,
    prompt: String,
    model_alias: Option<String>,
    plugins: Option<Vec<String>>,
    updated_at: DateTime<Utc>,
}

impl From<Row> for Definition {
    fn from(row: Row) -> Self {
        Definition {
            name: row.name,
            description: row.description,
            prompt: row.prompt,
            model_alias: row.model_alias,
            plugins: row.plugins,
            updated_at: Some(row.updated_at),
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = project_agent)]
struct NewRow<'a> {
    id: Uuid,
    project_id: Uuid,
    name: &'a str,
    description: &'a str,
    prompt: &'a str,
    model_alias: Option<&'a str>,
    plugins: Option<&'a [String]>,
}

#[derive(AsChangeset)]
#[diesel(table_name = project_agent)]
struct Changes<'a> {
    name: Option<&'a str>,
    description: Option<&'a str>,
    prompt: Option<&'a str>,
    model_alias: Option<Option<&'a str>>,
    plugins: Option<Option<&'a [String]>>,
    updated_at: DateTime<Utc>,
}

/// A new definition, as its owner wrote it.
#[derive(Clone, Debug, Default)]
pub struct New {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub model: Option<String>,
    pub plugins: Option<Vec<String>>,
}

/// What to change of a definition. `None` keeps a field; `Some(None)` clears
/// the model or the plugins back to the parent's.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub model: Option<Option<String>>,
    pub plugins: Option<Option<Vec<String>>>,
}

fn conflict(code: &'static str, message: String) -> AppError {
    AppError::Coded {
        status: axum::http::StatusCode::CONFLICT,
        code,
        message,
        details: None,
    }
}

fn builtin() -> AppError {
    conflict(
        "builtin",
        format!("`{GENERAL_PURPOSE}` is built in and cannot be changed or deleted"),
    )
}

fn check_name(name: &str) -> ApiResult<()> {
    let shaped = !name.is_empty()
        && name.len() <= NAME_CHARS
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !shaped {
        return Err(AppError::BadRequest(format!(
            "name must be 1 to {NAME_CHARS} lower-case letters, digits and hyphens, \
             and not start with a hyphen"
        )));
    }
    if name == GENERAL_PURPOSE {
        return Err(builtin());
    }
    Ok(())
}

fn check_description(description: &str) -> ApiResult<()> {
    if description.is_empty() {
        return Err(AppError::BadRequest(
            "description cannot be empty: it is how an agent decides to spawn this one".into(),
        ));
    }
    if description.chars().count() > DESCRIPTION_CHARS {
        return Err(AppError::BadRequest(format!(
            "description must be {DESCRIPTION_CHARS} characters or fewer"
        )));
    }
    Ok(())
}

fn check_prompt(prompt: &str) -> ApiResult<()> {
    if prompt.chars().count() > PROMPT_CHARS {
        return Err(AppError::BadRequest(format!(
            "prompt must be {PROMPT_CHARS} characters or fewer"
        )));
    }
    Ok(())
}

/// Refuses an alias the project's owner has no model under. Checked when a
/// definition is saved; a model deleted later is found out at spawn.
async fn check_model(conn: &mut AsyncPgConnection, owner: Uuid, alias: &str) -> ApiResult<()> {
    let known: Option<Uuid> = models::table
        .filter(models::user_id.eq(owner))
        .filter(models::alias.eq(alias))
        .select(models::id)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "definitions.model"))?;
    match known {
        Some(_) => Ok(()),
        None => Err(AppError::BadRequest(format!("you have no model `{alias}`"))),
    }
}

/// Refuses a plugin the project does not have bound, and returns the list
/// without repeats.
async fn check_plugins(
    conn: &mut AsyncPgConnection,
    project: Uuid,
    plugins: &[String],
) -> ApiResult<Vec<String>> {
    let bound: Vec<String> = project_binding::table
        .filter(project_binding::project_id.eq(project))
        .select(project_binding::plugin_type)
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "definitions.plugins"))?;
    let mut kept: Vec<String> = Vec::new();
    for plugin in plugins {
        if !bound.contains(plugin) {
            return Err(AppError::BadRequest(format!(
                "the project has no plugin `{plugin}`"
            )));
        }
        if !kept.contains(plugin) {
            kept.push(plugin.clone());
        }
    }
    Ok(kept)
}

fn name_taken(err: diesel::result::Error, context: &'static str) -> AppError {
    match err {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => conflict(
            "name-taken",
            "the project already has an agent with that name".into(),
        ),
        other => AppError::db(other, context),
    }
}

/// The project's definitions: the built-in first, then its own by name.
pub async fn list(conn: &mut AsyncPgConnection, project: Uuid) -> ApiResult<Vec<Definition>> {
    let rows: Vec<Row> = project_agent::table
        .filter(project_agent::project_id.eq(project))
        .order(project_agent::name.asc())
        .select(Row::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "definitions.list"))?;
    Ok(std::iter::once(Definition::general_purpose())
        .chain(rows.into_iter().map(Definition::from))
        .collect())
}

/// The definition a spawn names, if the project has it.
// Its first caller is the agent tools; until then only tests look one up.
#[allow(dead_code)]
pub async fn find(
    conn: &mut AsyncPgConnection,
    project: Uuid,
    name: &str,
) -> ApiResult<Option<Definition>> {
    if name == GENERAL_PURPOSE {
        return Ok(Some(Definition::general_purpose()));
    }
    let row: Option<Row> = project_agent::table
        .filter(project_agent::project_id.eq(project))
        .filter(project_agent::name.eq(name))
        .select(Row::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "definitions.find"))?;
    Ok(row.map(Definition::from))
}

/// Adds a definition to `project`, whose owner is `owner`.
pub async fn create(
    conn: &mut AsyncPgConnection,
    owner: Uuid,
    project: Uuid,
    new: New,
) -> ApiResult<Definition> {
    let name = new.name.trim();
    let description = new.description.trim();
    check_name(name)?;
    check_description(description)?;
    check_prompt(&new.prompt)?;
    let model = new.model.as_deref().map(str::trim);
    if let Some(alias) = model {
        check_model(conn, owner, alias).await?;
    }
    let plugins = match &new.plugins {
        Some(plugins) => Some(check_plugins(conn, project, plugins).await?),
        None => None,
    };
    let held: i64 = project_agent::table
        .filter(project_agent::project_id.eq(project))
        .count()
        .get_result(conn)
        .await
        .map_err(|err| AppError::db(err, "definitions.count"))?;
    if held >= PER_PROJECT {
        return Err(conflict(
            "too-many",
            format!("a project holds at most {PER_PROJECT} agent definitions"),
        ));
    }
    let row: Row = diesel::insert_into(project_agent::table)
        .values(NewRow {
            id: Uuid::now_v7(),
            project_id: project,
            name,
            description,
            prompt: &new.prompt,
            model_alias: model,
            plugins: plugins.as_deref(),
        })
        .returning(Row::as_returning())
        .get_result(conn)
        .await
        .map_err(|err| name_taken(err, "definitions.create"))?;
    Ok(row.into())
}

/// Changes the definition `name` of `project`.
pub async fn update(
    conn: &mut AsyncPgConnection,
    owner: Uuid,
    project: Uuid,
    name: &str,
    patch: Patch,
) -> ApiResult<Definition> {
    if name == GENERAL_PURPOSE {
        return Err(builtin());
    }
    let renamed = patch.name.as_deref().map(str::trim);
    if let Some(renamed) = renamed {
        check_name(renamed)?;
    }
    let description = patch.description.as_deref().map(str::trim);
    if let Some(description) = description {
        check_description(description)?;
    }
    if let Some(prompt) = &patch.prompt {
        check_prompt(prompt)?;
    }
    let model = patch
        .model
        .as_ref()
        .map(|model| model.as_deref().map(str::trim));
    if let Some(Some(alias)) = model {
        check_model(conn, owner, alias).await?;
    }
    let plugins = match &patch.plugins {
        Some(Some(plugins)) => Some(Some(check_plugins(conn, project, plugins).await?)),
        Some(None) => Some(None),
        None => None,
    };
    let row: Option<Row> = diesel::update(
        project_agent::table
            .filter(project_agent::project_id.eq(project))
            .filter(project_agent::name.eq(name)),
    )
    .set(Changes {
        name: renamed,
        description,
        prompt: patch.prompt.as_deref(),
        model_alias: model,
        plugins: plugins.as_ref().map(|plugins| plugins.as_deref()),
        updated_at: Utc::now(),
    })
    .returning(Row::as_returning())
    .get_result(conn)
    .await
    .optional()
    .map_err(|err| name_taken(err, "definitions.update"))?;
    row.map(Definition::from).ok_or(AppError::NotFound)
}

/// Deletes the definition `name` of `project`. Sessions spawned as it keep
/// the name and carry on.
pub async fn delete(conn: &mut AsyncPgConnection, project: Uuid, name: &str) -> ApiResult<()> {
    if name == GENERAL_PURPOSE {
        return Err(builtin());
    }
    let deleted = diesel::delete(
        project_agent::table
            .filter(project_agent::project_id.eq(project))
            .filter(project_agent::name.eq(name)),
    )
    .execute(conn)
    .await
    .map_err(|err| AppError::db(err, "definitions.delete"))?;
    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::daemon::fixture::Fixture;
    use diesel::sql_types::{Text, Uuid as SqlUuid};

    fn reviewer() -> New {
        New {
            name: "reviewer".into(),
            description: "Reviews a change for bugs.".into(),
            prompt: "You review code.".into(),
            ..New::default()
        }
    }

    async fn model(fixture: &Fixture, alias: &str) {
        let mut conn = fixture.pool.get().await.unwrap();
        diesel::sql_query(
            "INSERT INTO models (id, user_id, alias, base_url, wire, wire_id, params) \
             VALUES ($1, $2, $3, 'https://example.invalid', 'openai', 'm', '{}')",
        )
        .bind::<SqlUuid, _>(Uuid::now_v7())
        .bind::<SqlUuid, _>(fixture.user)
        .bind::<Text, _>(alias)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    async fn forget_models(fixture: &Fixture) {
        let mut conn = fixture.pool.get().await.unwrap();
        diesel::sql_query("DELETE FROM models WHERE user_id = $1")
            .bind::<SqlUuid, _>(fixture.user)
            .execute(&mut conn)
            .await
            .unwrap();
    }

    fn coded(result: ApiResult<impl std::fmt::Debug>) -> &'static str {
        match result {
            Err(AppError::Coded { code, .. }) => code,
            Err(AppError::BadRequest(_)) => "bad-request",
            Err(AppError::NotFound) => "not-found",
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_project_with_none_still_has_general_purpose() {
        let fixture = Fixture::new(0).await;
        let mut conn = fixture.pool.get().await.unwrap();
        let all = list(&mut conn, fixture.project).await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].builtin());
        assert_eq!(all[0].name, GENERAL_PURPOSE);
        assert_eq!((&all[0].model_alias, &all[0].plugins), (&None, &None));
        let found = find(&mut conn, fixture.project, GENERAL_PURPOSE).await;
        assert_eq!(found.unwrap().as_ref(), Some(&all[0]));

        // It is nobody's to take, change or delete.
        let (owner, project) = (fixture.user, fixture.project);
        let mut taken = reviewer();
        taken.name = GENERAL_PURPOSE.into();
        assert_eq!(
            coded(create(&mut conn, owner, project, taken).await),
            "builtin"
        );
        let patch = Patch::default();
        assert_eq!(
            coded(update(&mut conn, owner, project, GENERAL_PURPOSE, patch).await),
            "builtin"
        );
        assert_eq!(
            coded(delete(&mut conn, project, GENERAL_PURPOSE).await),
            "builtin"
        );
        drop(conn);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_owner_creates_edits_and_deletes_definitions() {
        let fixture = Fixture::new(1).await;
        model(&fixture, "fast").await;
        let (owner, project) = (fixture.user, fixture.project);
        let mut conn = fixture.pool.get().await.unwrap();
        diesel::sql_query(
            "INSERT INTO project_binding (project_id, plugin_type, version, config_version, config) \
             VALUES ($1, 'web', '1.0.0', 1, '{}')",
        )
        .bind::<SqlUuid, _>(project)
        .execute(&mut conn)
        .await
        .unwrap();

        let made = create(&mut conn, owner, project, reviewer()).await.unwrap();
        assert!(!made.builtin());
        assert_eq!((&made.model_alias, &made.plugins), (&None, &None));
        assert_eq!(
            coded(create(&mut conn, owner, project, reviewer()).await),
            "name-taken"
        );
        // The same name in another project is another definition.
        let elsewhere = fixture.project("other").await;
        create(&mut conn, owner, elsewhere, reviewer())
            .await
            .unwrap();

        for (name, description) in [("Reviewer", "x"), ("-r", "x"), ("", "x"), ("ok", " ")] {
            let mut bad = reviewer();
            bad.name = name.into();
            bad.description = description.into();
            assert_eq!(
                coded(create(&mut conn, owner, project, bad).await),
                "bad-request"
            );
        }

        // A model must be one the owner has, and a plugin one the project has.
        let with = |model: Option<&str>, plugins: Option<&[&str]>| Patch {
            model: Some(model.map(str::to_owned)),
            plugins: Some(plugins.map(|p| p.iter().map(|p| p.to_string()).collect())),
            ..Patch::default()
        };
        for patch in [with(Some("slow"), None), with(None, Some(&["discord"]))] {
            assert_eq!(
                coded(update(&mut conn, owner, project, "reviewer", patch).await),
                "bad-request"
            );
        }
        let patch = Patch {
            name: Some("critic".into()),
            prompt: Some("You find faults.".into()),
            ..with(Some("fast"), Some(&["web", "web"]))
        };
        let changed = update(&mut conn, owner, project, "reviewer", patch)
            .await
            .unwrap();
        assert_eq!(changed.name, "critic");
        assert_eq!(changed.description, "Reviews a change for bugs.");
        assert_eq!(changed.prompt, "You find faults.");
        assert_eq!(changed.model_alias.as_deref(), Some("fast"));
        assert_eq!(changed.plugins, Some(vec!["web".to_owned()]));
        assert_eq!(
            coded(update(&mut conn, owner, project, "reviewer", Patch::default()).await),
            "not-found"
        );
        // `null` hands both back to the parent.
        let cleared = update(&mut conn, owner, project, "critic", with(None, None))
            .await
            .unwrap();
        assert_eq!((&cleared.model_alias, &cleared.plugins), (&None, &None));
        let names = |all: Vec<Definition>| all.into_iter().map(|d| d.name).collect::<Vec<_>>();
        assert_eq!(
            names(list(&mut conn, project).await.unwrap()),
            [GENERAL_PURPOSE, "critic"]
        );

        // Deleting it leaves a session spawned as it where it was.
        let spawn = crate::children::Spawn {
            run: None,
            agent_type: "critic",
            name: None,
            model_alias: None,
            thinking_effort: None,
        };
        let child = crate::children::spawn(&mut conn, fixture.sessions[0], spawn)
            .await
            .unwrap();
        delete(&mut conn, project, "critic").await.unwrap();
        assert_eq!(
            coded(delete(&mut conn, project, "critic").await),
            "not-found"
        );
        assert_eq!(find(&mut conn, project, "critic").await.unwrap(), None);
        let children = crate::children::of(&mut conn, fixture.sessions[0]).await;
        let children = children.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);
        assert_eq!(children[0].agent_type.as_deref(), Some("critic"));
        assert_eq!(children[0].closed_at, None);

        drop(conn);
        forget_models(&fixture).await;
        fixture.cleanup().await;
    }
}
