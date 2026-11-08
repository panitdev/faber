use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, post},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use russh::keys::decode_secret_key;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    crypto::encrypt_key,
    error::{ApiResult, AppError},
    models::credential::{Credential, NewCredential},
    schema::credentials,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/credentials", post(create).get(list))
        .route("/api/credentials/{id}", delete(remove))
}

#[derive(Deserialize)]
struct CreateRequest {
    label: String,
    kind: CredentialKind,
    key: String,
}

#[derive(Deserialize)]
struct ListQuery {
    kind: Option<CredentialKind>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialKind {
    ApiKey,
    SshKey,
}

impl CredentialKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::SshKey => "ssh_key",
        }
    }
}

#[derive(Serialize)]
struct CredentialResponse {
    id: Uuid,
    label: String,
    kind: String,
    last_four: String,
    created_at: DateTime<Utc>,
}

fn credential_response(c: &Credential) -> CredentialResponse {
    CredentialResponse {
        id: c.id,
        label: c.label.clone(),
        kind: c.kind.clone(),
        last_four: c.last_four.clone(),
        created_at: c.created_at,
    }
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<CredentialResponse>)> {
    let label = input.label.trim();
    if label.is_empty() {
        return Err(AppError::BadRequest("label is required".into()));
    }
    if label.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "label must be 100 characters or fewer".into(),
        ));
    }

    let key = input.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest("key is required".into()));
    }

    if matches!(input.kind, CredentialKind::SshKey) {
        decode_secret_key(key, None).map_err(|error| {
            AppError::BadRequest(format!(
                "SSH credential must contain a valid unencrypted private key: {error}"
            ))
        })?;
    }

    let last_four: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    let cred_id = Uuid::now_v7();
    let (ciphertext, nonce) = encrypt_key(
        key.as_bytes(),
        state.master_key.as_bytes(),
        cred_id,
        user.id,
    )
    .map_err(|error| AppError::internal("credentials.encrypt", error))?;

    let new_cred = NewCredential {
        id: cred_id,
        user_id: user.id,
        label,
        kind: input.kind.as_str(),
        key_ciphertext: &ciphertext,
        key_nonce: &nonce,
        key_version: "v1",
        last_four: &last_four,
    };

    let mut conn = state.db.get().await?;

    let inserted: Credential = diesel::insert_into(credentials::table)
        .values(&new_cred)
        .returning(Credential::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a credential named '{label}' already exists")),
            other => AppError::db(other, "credentials.create"),
        })?;

    Ok((StatusCode::CREATED, Json(credential_response(&inserted))))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<CredentialResponse>>> {
    let mut conn = state.db.get().await?;

    let mut statement = credentials::table
        .filter(credentials::user_id.eq(user.id))
        .into_boxed();
    if let Some(kind) = query.kind {
        statement = statement.filter(credentials::kind.eq(kind.as_str()));
    }
    let rows: Vec<Credential> = statement
        .order(credentials::created_at.asc())
        .select(Credential::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "credentials.list"))?;

    Ok(Json(rows.iter().map(credential_response).collect()))
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        credentials::table
            .filter(credentials::id.eq(id))
            .filter(credentials::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "credentials.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}
