//! The model presets, owned by a user or by the system.
//!
//! A preset is one provider serving one model — what it costs there, and how
//! that provider describes it — and is not a row a run calls. `user_id IS
//! NULL` marks a system preset: the catalog the service fetches at boot,
//! shared by every user and refreshed on each load. A row with `user_id` set
//! is a preset the user wrote, private to them. The two live in one table so a
//! per-user listing can read both in one query, while the ownership column
//! keeps "the user chose this" and "we fetched this" distinguishable.
//!
//! A preset may link to a [`crate::models::creator_model`] — the model as its
//! creator describes it — and then stores only what it says differently: every
//! descriptive column is an override, `NULL` meaning "as the creator model
//! says". An unlinked preset reads its overrides over an empty description, so
//! it states everything itself. [`resolve`] is the one place the two are laid
//! together.
//!
//! A preset references a [`crate::models::model_provider`] and carries the same
//! owner as it. The API enforces that pairing on every write; it is what lets
//! [`replace_all`] refresh the system half without touching anybody's rows.
//! Creator models have no owner — they are all the system's — so a user's
//! preset may link to any of them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    models::{
        creator_model::{CreatorModelRow, NewCreatorModel, from_db, to_db},
        model_provider::NewModelProvider,
    },
    schema::{creator_models, model_preset_rebinds, model_presets, model_providers},
};

/// Rows per `INSERT`, for each of the three tables [`replace_all`] writes.
/// PostgreSQL allows 65535 bind parameters per statement; a preset's 23
/// inserted columns — the most of the three — cap one statement near 2800
/// rows, and the catalog publishes far more presets than that.
const INSERT_CHUNK: usize = 1000;

/// One preset as stored.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = model_presets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ModelPresetRow {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub model_provider_id: Uuid,
    /// The model id as served, e.g. `claude-opus-5`.
    pub model_id: String,
    pub creator_model_id: Option<Uuid>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub family: Option<String>,
    pub attachment: Option<bool>,
    pub reasoning: Option<bool>,
    pub tool_call: Option<bool>,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub open_weights: Option<bool>,
    pub limits: Option<Value>,
    pub modalities: Option<Value>,
    pub cost: Option<Value>,
    pub reasoning_options: Option<Value>,
    pub interleaved: Option<Value>,
    pub status: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ModelPresetRow {
    /// What this preset says differently from its creator model — or, with no
    /// creator model, everything it says.
    pub fn overrides(&self) -> presets::Overrides {
        presets::Overrides {
            name: self.name.clone(),
            description: self.description.clone(),
            family: self.family.clone(),
            attachment: self.attachment,
            reasoning: self.reasoning,
            tool_call: self.tool_call,
            structured_output: self.structured_output,
            temperature: self.temperature,
            knowledge: self.knowledge.clone(),
            release_date: self.release_date.clone(),
            last_updated: self.last_updated.clone(),
            open_weights: self.open_weights,
            limit: self.limits.as_ref().map(from_db),
            modalities: self.modalities.as_ref().map(from_db),
        }
    }

    pub fn serving(&self) -> presets::Serving {
        presets::Serving {
            cost: self.cost.as_ref().map(from_db),
            reasoning_options: self.reasoning_options.clone(),
            interleaved: self.interleaved.clone(),
            status: self.status.clone(),
        }
    }
}

/// A preset as the caller shows it: its overrides laid over the creator model
/// it links to, with the provider it is served by. `creator` is `None` for an
/// unlinked preset, and for a linked one whose creator row the caller did not
/// load — the preset then reads as though unlinked, stating only its own
/// overrides.
pub fn resolve(
    row: &ModelPresetRow,
    provider: &str,
    provider_name: &str,
    creator: Option<&CreatorModelRow>,
) -> presets::Preset {
    let base = creator.map(CreatorModelRow::to_spec);
    presets::Preset {
        provider: provider.to_owned(),
        provider_name: provider_name.to_owned(),
        id: row.model_id.clone(),
        base_model: creator.map(|creator| creator.model_id.clone()),
        spec: row.overrides().apply(base.as_ref(), &row.model_id),
        serving: row.serving(),
    }
}

/// The columns a preset read selects: the row, its provider's key and display
/// name, and the creator model it links to, if any.
pub type Joined = (ModelPresetRow, String, String, Option<CreatorModelRow>);

/// Resolves a [`Joined`] read.
pub fn resolve_joined((row, provider, provider_name, creator): &Joined) -> presets::Preset {
    resolve(row, provider, provider_name, creator.as_ref())
}

/// The presets among `ids` that `owner` can see — their own and the
/// system's — resolved, by id. An id that names nothing visible is absent.
pub async fn load_visible(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    owner: Uuid,
) -> QueryResult<HashMap<Uuid, (ModelPresetRow, presets::Preset)>> {
    let rows: Vec<Joined> = model_presets::table
        .inner_join(model_providers::table)
        .left_join(creator_models::table)
        .filter(model_presets::id.eq_any(ids))
        .filter(
            model_presets::user_id
                .eq(owner)
                .or(model_presets::user_id.is_null()),
        )
        .select((
            ModelPresetRow::as_select(),
            model_providers::provider_id,
            model_providers::name,
            Option::<CreatorModelRow>::as_select(),
        ))
        .load(conn)
        .await?;

    Ok(rows
        .into_iter()
        .map(|joined| {
            let preset = resolve_joined(&joined);
            (joined.0.id, (joined.0, preset))
        })
        .collect())
}

/// One preset as it is written.
#[derive(Debug, Insertable)]
#[diesel(table_name = model_presets)]
pub struct NewModelPreset {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub model_provider_id: Uuid,
    pub model_id: String,
    pub creator_model_id: Option<Uuid>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub family: Option<String>,
    pub attachment: Option<bool>,
    pub reasoning: Option<bool>,
    pub tool_call: Option<bool>,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub open_weights: Option<bool>,
    pub limits: Option<Value>,
    pub modalities: Option<Value>,
    pub cost: Option<Value>,
    pub reasoning_options: Option<Value>,
    pub interleaved: Option<Value>,
    pub status: Option<String>,
}

impl NewModelPreset {
    /// A preset: `user_id` is `None` for the system's. The id names the row
    /// only when it is inserted — [`replace_all`] upserts, and a row the
    /// catalog already has keeps its own.
    pub fn new(
        id: Uuid,
        user_id: Option<Uuid>,
        model_provider_id: Uuid,
        model_id: String,
        creator_model_id: Option<Uuid>,
        overrides: &presets::Overrides,
        serving: &presets::Serving,
    ) -> Self {
        Self {
            id,
            user_id,
            model_provider_id,
            model_id,
            creator_model_id,
            name: overrides.name.clone(),
            description: overrides.description.clone(),
            family: overrides.family.clone(),
            attachment: overrides.attachment,
            reasoning: overrides.reasoning,
            tool_call: overrides.tool_call,
            structured_output: overrides.structured_output,
            temperature: overrides.temperature,
            knowledge: overrides.knowledge.clone(),
            release_date: overrides.release_date.clone(),
            last_updated: overrides.last_updated.clone(),
            open_weights: overrides.open_weights,
            limits: overrides.limit.as_ref().map(to_db),
            modalities: overrides.modalities.as_ref().map(to_db),
            cost: serving.cost.as_ref().map(to_db),
            reasoning_options: serving.reasoning_options.clone(),
            interleaved: serving.interleaved.clone(),
            status: serving.status.clone(),
        }
    }
}

/// The fields a `PATCH` may change on a user's preset. An absent field is left
/// alone; a nullable one given as `null` is cleared.
#[derive(AsChangeset, Default)]
#[diesel(table_name = model_presets)]
pub struct UpdateModelPreset {
    pub model_provider_id: Option<Uuid>,
    pub model_id: Option<String>,
    pub creator_model_id: Option<Option<Uuid>>,
    pub name: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub family: Option<Option<String>>,
    pub attachment: Option<Option<bool>>,
    pub reasoning: Option<Option<bool>>,
    pub tool_call: Option<Option<bool>>,
    pub structured_output: Option<Option<bool>>,
    pub temperature: Option<Option<bool>>,
    pub knowledge: Option<Option<String>>,
    pub release_date: Option<Option<String>>,
    pub last_updated: Option<Option<String>>,
    pub open_weights: Option<Option<bool>>,
    pub limits: Option<Option<Value>>,
    pub modalities: Option<Option<Value>>,
    pub cost: Option<Option<Value>>,
    pub reasoning_options: Option<Option<Value>>,
    pub interleaved: Option<Option<Value>>,
    pub status: Option<Option<String>>,
}

impl UpdateModelPreset {
    /// Replaces every override at once: a field `overrides` leaves unset is
    /// cleared back to "as the creator model says", not left as it was.
    pub fn set_overrides(&mut self, overrides: &presets::Overrides) {
        self.name = Some(overrides.name.clone());
        self.description = Some(overrides.description.clone());
        self.family = Some(overrides.family.clone());
        self.attachment = Some(overrides.attachment);
        self.reasoning = Some(overrides.reasoning);
        self.tool_call = Some(overrides.tool_call);
        self.structured_output = Some(overrides.structured_output);
        self.temperature = Some(overrides.temperature);
        self.knowledge = Some(overrides.knowledge.clone());
        self.release_date = Some(overrides.release_date.clone());
        self.last_updated = Some(overrides.last_updated.clone());
        self.open_weights = Some(overrides.open_weights);
        self.limits = Some(overrides.limit.as_ref().map(to_db));
        self.modalities = Some(overrides.modalities.as_ref().map(to_db));
    }
}

/// Syncs the system half of the catalog tables from `catalog`, atomically.
///
/// An upsert keyed on each table's natural key — `(provider_id)` for a
/// provider, `(model_id)` for a creator model, `(model_provider_id,
/// model_id)` for a preset, the first and last under `user_id IS NULL` —
/// rather than delete-then-insert: a row the catalog still publishes keeps its
/// id across refreshes, which is what lets a model point at a preset, and a
/// preset at a creator model, and keep pointing at it across a restart. A key
/// the catalog stops publishing is left as it is; a user's preset may link to
/// a creator model the catalog has dropped, and an orphan row costs only a
/// listing nobody reads. Only system rows are touched — a user's rows are left
/// alone. The caller decides what a failure means; the previous rows are left
/// untouched when this returns an error.
///
/// The id each row is written with is its identity on insert only: a row that
/// already exists keeps its id and its `created_at`, while every other field
/// is overwritten from the catalog. Providers and creator models are upserted
/// first and read back, so presets are keyed by the rows that already exist
/// and a system preset always points at a system provider.
pub async fn replace_all(
    conn: &mut AsyncPgConnection,
    catalog: &presets::Catalog,
) -> QueryResult<()> {
    use diesel::upsert::{DecoratableTarget, excluded};
    use diesel_async::scoped_futures::ScopedFutureExt;

    let new_providers: Vec<NewModelProvider> = catalog
        .providers()
        .iter()
        .map(|provider| NewModelProvider {
            id: Uuid::now_v7(),
            user_id: None,
            provider_id: provider.id.clone(),
            name: provider.name.clone(),
            doc: provider.doc.clone(),
            api: provider.api.clone(),
            npm: provider.npm.clone(),
            env: to_db(&provider.env),
        })
        .collect();

    let new_models: Vec<NewCreatorModel> = catalog
        .models()
        .iter()
        .map(|model| NewCreatorModel::new(Uuid::now_v7(), model))
        .collect();

    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        async move {
            // Providers and creator models first: a preset's conflict key
            // holds the provider row id and its link holds the creator row id,
            // so the upserts that leave existing ids alone are also what keep
            // every preset's key and link stable across a refresh.
            let mut provider_ids: HashMap<String, Uuid> = HashMap::new();
            for chunk in new_providers.chunks(INSERT_CHUNK) {
                let written: Vec<(String, Uuid)> = diesel::insert_into(model_providers::table)
                    .values(chunk)
                    .on_conflict(model_providers::provider_id)
                    .filter_target(model_providers::user_id.is_null())
                    .do_update()
                    .set((
                        model_providers::name.eq(excluded(model_providers::name)),
                        model_providers::doc.eq(excluded(model_providers::doc)),
                        model_providers::api.eq(excluded(model_providers::api)),
                        model_providers::npm.eq(excluded(model_providers::npm)),
                        model_providers::env.eq(excluded(model_providers::env)),
                    ))
                    .returning((model_providers::provider_id, model_providers::id))
                    .load(conn)
                    .await?;
                provider_ids.extend(written);
            }

            let mut model_ids: HashMap<String, Uuid> = HashMap::new();
            for chunk in new_models.chunks(INSERT_CHUNK) {
                let written: Vec<(String, Uuid)> = diesel::insert_into(creator_models::table)
                    .values(chunk)
                    .on_conflict(creator_models::model_id)
                    .do_update()
                    .set((
                        creator_models::creator.eq(excluded(creator_models::creator)),
                        creator_models::name.eq(excluded(creator_models::name)),
                        creator_models::description.eq(excluded(creator_models::description)),
                        creator_models::family.eq(excluded(creator_models::family)),
                        creator_models::attachment.eq(excluded(creator_models::attachment)),
                        creator_models::reasoning.eq(excluded(creator_models::reasoning)),
                        creator_models::tool_call.eq(excluded(creator_models::tool_call)),
                        creator_models::structured_output
                            .eq(excluded(creator_models::structured_output)),
                        creator_models::temperature.eq(excluded(creator_models::temperature)),
                        creator_models::knowledge.eq(excluded(creator_models::knowledge)),
                        creator_models::release_date.eq(excluded(creator_models::release_date)),
                        creator_models::last_updated.eq(excluded(creator_models::last_updated)),
                        creator_models::open_weights.eq(excluded(creator_models::open_weights)),
                        creator_models::limits.eq(excluded(creator_models::limits)),
                        creator_models::modalities.eq(excluded(creator_models::modalities)),
                        creator_models::license.eq(excluded(creator_models::license)),
                    ))
                    .returning((creator_models::model_id, creator_models::id))
                    .load(conn)
                    .await?;
                model_ids.extend(written);
            }

            let new_presets: Vec<NewModelPreset> = catalog
                .offerings()
                .iter()
                .filter_map(|offering| {
                    let provider_id = *provider_ids.get(offering.provider.as_str())?;
                    let creator_model_id = offering
                        .base_model
                        .as_deref()
                        .and_then(|id| model_ids.get(id).copied());
                    Some(NewModelPreset::new(
                        Uuid::now_v7(),
                        None,
                        provider_id,
                        offering.id.clone(),
                        creator_model_id,
                        &offering.overrides,
                        &offering.serving,
                    ))
                })
                .collect();

            for chunk in new_presets.chunks(INSERT_CHUNK) {
                diesel::insert_into(model_presets::table)
                    .values(chunk)
                    .on_conflict((model_presets::model_provider_id, model_presets::model_id))
                    .filter_target(model_presets::user_id.is_null())
                    .do_update()
                    .set((
                        // Every other column mirrors the catalog; the id, the
                        // owner, the conflict key, and `created_at` stay as
                        // they are — see the doc above.
                        model_presets::creator_model_id
                            .eq(excluded(model_presets::creator_model_id)),
                        model_presets::name.eq(excluded(model_presets::name)),
                        model_presets::description.eq(excluded(model_presets::description)),
                        model_presets::family.eq(excluded(model_presets::family)),
                        model_presets::attachment.eq(excluded(model_presets::attachment)),
                        model_presets::reasoning.eq(excluded(model_presets::reasoning)),
                        model_presets::tool_call.eq(excluded(model_presets::tool_call)),
                        model_presets::structured_output
                            .eq(excluded(model_presets::structured_output)),
                        model_presets::temperature.eq(excluded(model_presets::temperature)),
                        model_presets::knowledge.eq(excluded(model_presets::knowledge)),
                        model_presets::release_date.eq(excluded(model_presets::release_date)),
                        model_presets::last_updated.eq(excluded(model_presets::last_updated)),
                        model_presets::open_weights.eq(excluded(model_presets::open_weights)),
                        model_presets::limits.eq(excluded(model_presets::limits)),
                        model_presets::modalities.eq(excluded(model_presets::modalities)),
                        model_presets::cost.eq(excluded(model_presets::cost)),
                        model_presets::reasoning_options
                            .eq(excluded(model_presets::reasoning_options)),
                        model_presets::interleaved.eq(excluded(model_presets::interleaved)),
                        model_presets::status.eq(excluded(model_presets::status)),
                    ))
                    .execute(conn)
                    .await?;
            }

            // Bindings a migration wrote down when it replaced the catalog a
            // model was bound to: each goes to the new system preset under the
            // same provider key and served id, when the catalog has one. A
            // model rebound since then is left alone. Emptied either way — a
            // key this load does not publish will not appear on a later one
            // any more than it did on this.
            diesel::sql_query(
                "UPDATE models m SET preset_id = p.id \
                 FROM model_preset_rebinds r \
                 JOIN model_providers pr \
                   ON pr.provider_id = r.provider_key AND pr.user_id IS NULL \
                 JOIN model_presets p \
                   ON p.model_provider_id = pr.id AND p.model_id = r.served_id \
                   AND p.user_id IS NULL \
                 WHERE m.id = r.model_id AND m.preset_id IS NULL",
            )
            .execute(conn)
            .await?;
            diesel::delete(model_preset_rebinds::table)
                .execute(conn)
                .await?;
            Ok(())
        }
        .scope_boxed()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> ModelPresetRow {
        ModelPresetRow {
            id: Uuid::nil(),
            user_id: None,
            model_provider_id: Uuid::nil(),
            model_id: "claude-opus-5".into(),
            creator_model_id: None,
            name: None,
            description: None,
            family: None,
            attachment: None,
            reasoning: None,
            tool_call: None,
            structured_output: None,
            temperature: None,
            knowledge: None,
            release_date: None,
            last_updated: None,
            open_weights: None,
            limits: Some(serde_json::json!({ "context": 200000, "output": 64000 })),
            modalities: None,
            cost: Some(serde_json::json!({ "input": 3, "output": 15, "tiers": [] })),
            reasoning_options: None,
            interleaved: None,
            status: None,
            created_at: Utc::now(),
        }
    }

    fn creator() -> CreatorModelRow {
        CreatorModelRow {
            id: Uuid::nil(),
            model_id: "anthropic/claude-opus-5".into(),
            creator: "anthropic".into(),
            name: "Claude Opus 5".into(),
            description: Some("Frontier model".into()),
            family: Some("claude-opus".into()),
            attachment: true,
            reasoning: true,
            tool_call: true,
            structured_output: Some(true),
            temperature: Some(true),
            knowledge: Some("2026-01".into()),
            release_date: Some("2026-03-01".into()),
            last_updated: None,
            open_weights: Some(false),
            limits: serde_json::json!({ "context": 1000000, "output": 128000 }),
            modalities: serde_json::json!({ "input": ["text", "image"], "output": ["text"] }),
            license: None,
        }
    }

    #[test]
    fn a_linked_preset_reads_its_creator_model_under_its_own_overrides() {
        let preset = resolve(&row(), "router", "Router", Some(&creator()));

        assert_eq!(preset.provider, "router");
        assert_eq!(preset.id, "claude-opus-5");
        assert_eq!(preset.base_model.as_deref(), Some("anthropic/claude-opus-5"));
        // Inherited from the creator model.
        assert_eq!(preset.spec.name, "Claude Opus 5");
        assert!(preset.spec.reasoning);
        assert!(preset.spec.vision());
        // The provider's own.
        assert_eq!(preset.spec.limit.context, Some(200_000));
        let cost = preset.serving.cost.unwrap();
        assert_eq!(cost.input, Some(3.0));
        assert!(cost.other.contains_key("tiers"));
    }

    #[test]
    fn an_unlinked_preset_states_only_what_it_stores() {
        let preset = resolve(&row(), "router", "Router", None);
        assert_eq!(preset.base_model, None);
        assert_eq!(preset.spec.name, "claude-opus-5");
        assert!(!preset.spec.reasoning);
        assert_eq!(preset.spec.limit.context, Some(200_000));
    }

    #[test]
    fn the_insert_shape_stores_the_overrides_it_was_given() {
        let overrides = presets::Overrides {
            reasoning: Some(false),
            limit: Some(presets::Limit {
                context: Some(8192),
                ..Default::default()
            }),
            ..Default::default()
        };
        let new = NewModelPreset::new(
            Uuid::nil(),
            Some(Uuid::nil()),
            Uuid::nil(),
            "tiny".into(),
            None,
            &overrides,
            &presets::Serving::default(),
        );
        assert_eq!(new.reasoning, Some(false));
        assert_eq!(new.name, None);
        assert_eq!(new.limits, Some(serde_json::json!({ "context": 8192, "input": null, "output": null })));
        assert_eq!(new.cost, None);
    }

    #[test]
    fn replacing_overrides_clears_the_ones_left_unset() {
        let mut patch = UpdateModelPreset::default();
        patch.set_overrides(&presets::Overrides {
            name: Some("Mine".into()),
            ..Default::default()
        });
        assert_eq!(patch.name, Some(Some("Mine".into())));
        assert_eq!(patch.reasoning, Some(None));
        assert_eq!(patch.limits, Some(None));
    }
}
