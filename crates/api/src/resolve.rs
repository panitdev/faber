use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use crate::{
    crypto::decrypt_key,
    error::{ApiResult, AppError},
    models::{
        credential::Credential, host::Host, model_config::ModelConfig,
        model_preset::load_visible, thinking::ThinkingCapability,
    },
    schema::{credentials, models},
    state::AppState,
};

pub struct ResolvedModel {
    pub config: ModelConfig,
    pub api_key: String,
    /// The thinking knob a run on this model is read against — the row's own,
    /// or its preset's; see [`ModelConfig::effective_thinking`].
    pub thinking: ThinkingCapability,
}

/// A host's SSH configuration with the key material actually in hand.
///
/// The `environment` crate takes material and never a reference, because it
/// does not know what a secret store is and teaching it would point a
/// dependency the wrong way — the foreign keys run from the harness toward
/// environments, so resolution belongs on this side of that line.
pub struct ResolvedHostKey {
    pub credential_id: Uuid,
    pub address: String,
    pub user: String,
    pub private_key: String,
    /// The fingerprint this host is known by, or `None` on first contact.
    /// `None` means the next connection has to record what it sees; `Some`
    /// means anything else is refused.
    pub host_key: Option<String>,
}

/// Resolves `alias` to a decrypted API key for `user_id`.
///
/// Returns `NotFound` if the model does not exist or belongs to a different user.
/// Returns `Internal` if decryption fails — a corrupt or replaced key is not surfaced in detail.
pub async fn resolve_model(
    state: &AppState,
    user_id: Uuid,
    alias: &str,
) -> ApiResult<ResolvedModel> {
    let mut conn = state.db.get().await?;

    let model: ModelConfig = models::table
        .filter(models::user_id.eq(user_id))
        .filter(models::alias.eq(alias))
        .select(ModelConfig::as_select())
        .first(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "resolve.model_by_alias"))?;

    let cred_id = model
        .credential_id
        .ok_or_else(|| AppError::BadRequest("model has no credential attached".into()))?;

    let cred: Credential = credentials::table
        .filter(credentials::id.eq(cred_id))
        .filter(credentials::user_id.eq(user_id))
        .select(Credential::as_select())
        .first(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "resolve.credential_lookup"))?;

    let key_bytes = decrypt_key(
        &cred.key_ciphertext,
        &cred.key_nonce,
        state.master_key.as_bytes(),
        cred.id,
        user_id,
    )
    .map_err(|_| AppError::Internal)?;

    let api_key = String::from_utf8(key_bytes).map_err(|_| AppError::Internal)?;

    let preset = match model.preset_id {
        Some(preset_id) => load_visible(&mut conn, &[preset_id], user_id)
            .await
            .map_err(|err| AppError::db(err, "resolve.preset"))?
            .remove(&preset_id)
            .map(|(_, preset)| preset),
        None => None,
    };
    let thinking = model.effective_thinking(preset.as_ref());

    Ok(ResolvedModel {
        config: model,
        api_key,
        thinking,
    })
}

/// Resolves an SSH host's stored credential reference into key material.
///
/// `ssh_address` is `user@host:port`; the user travels with the address
/// because it is part of naming the account rather than part of the secret.
/// A host with no `ssh_key_ref` is a configuration error rather than an
/// invitation to try another authentication method — password and agent
/// authentication are both things this service must not fall back to, since
/// neither is the user's own.
pub async fn resolve_host_key(
    state: &AppState,
    user_id: Uuid,
    host: &Host,
) -> ApiResult<ResolvedHostKey> {
    if host.transport != "ssh" {
        return Err(AppError::BadRequest(
            "only an ssh host has a key to resolve".into(),
        ));
    }
    if host.disabled_at.is_some() {
        return Err(AppError::BadRequest("this host is disabled".into()));
    }

    let address = host
        .ssh_address
        .clone()
        .ok_or_else(|| AppError::BadRequest("this host has no ssh_address".into()))?;
    let (user, address) = address
        .split_once('@')
        .ok_or_else(|| AppError::BadRequest("ssh_address is user@host:port".into()))?;

    let reference = host
        .ssh_key_ref
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("this host has no ssh_key_ref".into()))?;
    let cred_id = Uuid::parse_str(reference)
        .map_err(|_| AppError::BadRequest("ssh_key_ref is not a credential id".into()))?;

    let mut conn = state.db.get().await?;
    let cred: Credential = credentials::table
        .filter(credentials::id.eq(cred_id))
        .filter(credentials::user_id.eq(user_id))
        .select(Credential::as_select())
        .first(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "resolve.host_key_lookup"))?;

    let key_bytes = decrypt_key(
        &cred.key_ciphertext,
        &cred.key_nonce,
        state.master_key.as_bytes(),
        cred.id,
        user_id,
    )
    .map_err(|_| AppError::Internal)?;

    Ok(ResolvedHostKey {
        credential_id: cred.id,
        address: address.to_owned(),
        user: user.to_owned(),
        private_key: String::from_utf8(key_bytes).map_err(|_| AppError::Internal)?,
        host_key: host.ssh_host_key.clone(),
    })
}
