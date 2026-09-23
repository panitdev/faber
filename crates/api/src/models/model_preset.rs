//! The model presets, owned by a user or by the system.
//!
//! A preset is a third party's description of a model — what it can do and what
//! it costs — and is not a row a run calls. `user_id IS NULL` marks a system
//! preset: the catalog the service fetches at boot, shared by every user and
//! refreshed on each load. A row with `user_id` set is a preset the user wrote,
//! private to them. The two live in one table so a per-user listing can read
//! both in one query, while the ownership column keeps "the user chose this"
//! and "we fetched this" distinguishable.
//!
//! A preset references a [`crate::models::model_provider`] and carries the same
//! owner as it. The API enforces that pairing on every write; it is what lets
//! [`replace_all`] refresh the system half without touching anybody's rows.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    models::model_provider::NewModelProvider,
    schema::{model_presets, model_providers},
};

/// Rows per `INSERT`. PostgreSQL allows 65535 bind parameters per statement;
/// with this table's 27 inserted columns that caps one statement near 2400
/// rows, and the directory publishes far more than that.
const INSERT_CHUNK: usize = 1000;

/// One preset as stored. Field order matches the table's column order, which
/// `#[derive(Selectable)]` requires.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = model_presets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ModelPresetRow {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub model_provider_id: Uuid,
    pub model_id: String,
    pub name: String,
    pub vision: bool,
    pub attachment: bool,
    pub reasoning: bool,
    pub tools: bool,
    pub structured_output: bool,
    pub temperature: bool,
    pub price_input: Option<f64>,
    pub price_output: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
    pub price_input_audio: Option<f64>,
    pub price_output_audio: Option<f64>,
    pub price_reasoning: Option<f64>,
    pub limit_context: Option<i64>,
    pub limit_input: Option<i64>,
    pub limit_output: Option<i64>,
    pub modalities_input: Value,
    pub modalities_output: Value,
    pub release_date: Option<i64>,
    pub last_updated: Option<i64>,
    pub knowledge_cutoff: Option<i64>,
    pub open_weights: Option<bool>,
    pub created_at: DateTime<Utc>,
}

impl ModelPresetRow {
    /// The published shape, given the provider fields the row joins to. A
    /// preset stores only the provider it references; the key and display name
    /// live on the provider.
    pub fn to_preset(&self, provider: &str, provider_name: &str) -> presets::Preset {
        presets::Preset {
            provider: provider.to_owned(),
            provider_name: provider_name.to_owned(),
            id: self.model_id.clone(),
            name: self.name.clone(),
            capabilities: presets::Capabilities {
                vision: self.vision,
                attachment: self.attachment,
                reasoning: self.reasoning,
                tools: self.tools,
                structured_output: self.structured_output,
                temperature: self.temperature,
            },
            pricing: presets::Pricing {
                input: self.price_input,
                output: self.price_output,
                cache_read: self.price_cache_read,
                cache_write: self.price_cache_write,
                input_audio: self.price_input_audio,
                output_audio: self.price_output_audio,
                reasoning: self.price_reasoning,
            },
            limits: presets::Limits {
                context: limit_from_db(self.limit_context),
                input: limit_from_db(self.limit_input),
                output: limit_from_db(self.limit_output),
            },
            modalities: presets::Modalities {
                input: modalities_from_db(&self.modalities_input),
                output: modalities_from_db(&self.modalities_output),
            },
            release_date: self.release_date,
            last_updated: self.last_updated,
            knowledge_cutoff: self.knowledge_cutoff,
            open_weights: self.open_weights,
        }
    }
}

/// One preset as it is written.
#[derive(Debug, Insertable)]
#[diesel(table_name = model_presets)]
pub struct NewModelPreset {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub model_provider_id: Uuid,
    pub model_id: String,
    pub name: String,
    pub vision: bool,
    pub attachment: bool,
    pub reasoning: bool,
    pub tools: bool,
    pub structured_output: bool,
    pub temperature: bool,
    pub price_input: Option<f64>,
    pub price_output: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
    pub price_input_audio: Option<f64>,
    pub price_output_audio: Option<f64>,
    pub price_reasoning: Option<f64>,
    pub limit_context: Option<i64>,
    pub limit_input: Option<i64>,
    pub limit_output: Option<i64>,
    pub modalities_input: Value,
    pub modalities_output: Value,
    pub release_date: Option<i64>,
    pub last_updated: Option<i64>,
    pub knowledge_cutoff: Option<i64>,
    pub open_weights: Option<bool>,
}

impl NewModelPreset {
    /// A system preset: the boot-load shape, with no owner. The id names the
    /// row only when it is inserted — [`replace_all`] upserts, and a row the
    /// catalog already has keeps its own.
    pub fn system(id: Uuid, model_provider_id: Uuid, preset: &presets::Preset) -> Self {
        Self::build(id, None, model_provider_id, preset)
    }

    /// A preset the user wrote, under one of their own providers.
    pub fn owned(id: Uuid, owner: Uuid, model_provider_id: Uuid, preset: &presets::Preset) -> Self {
        Self::build(id, Some(owner), model_provider_id, preset)
    }

    fn build(
        id: Uuid,
        user_id: Option<Uuid>,
        model_provider_id: Uuid,
        preset: &presets::Preset,
    ) -> Self {
        Self {
            id,
            user_id,
            model_provider_id,
            model_id: preset.id.clone(),
            name: preset.name.clone(),
            vision: preset.capabilities.vision,
            attachment: preset.capabilities.attachment,
            reasoning: preset.capabilities.reasoning,
            tools: preset.capabilities.tools,
            structured_output: preset.capabilities.structured_output,
            temperature: preset.capabilities.temperature,
            price_input: preset.pricing.input,
            price_output: preset.pricing.output,
            price_cache_read: preset.pricing.cache_read,
            price_cache_write: preset.pricing.cache_write,
            price_input_audio: preset.pricing.input_audio,
            price_output_audio: preset.pricing.output_audio,
            price_reasoning: preset.pricing.reasoning,
            limit_context: limit_to_db(preset.limits.context),
            limit_input: limit_to_db(preset.limits.input),
            limit_output: limit_to_db(preset.limits.output),
            modalities_input: modalities_to_db(&preset.modalities.input),
            modalities_output: modalities_to_db(&preset.modalities.output),
            release_date: preset.release_date,
            last_updated: preset.last_updated,
            knowledge_cutoff: preset.knowledge_cutoff,
            open_weights: preset.open_weights,
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
    pub name: Option<String>,
    pub vision: Option<bool>,
    pub attachment: Option<bool>,
    pub reasoning: Option<bool>,
    pub tools: Option<bool>,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    pub price_input: Option<Option<f64>>,
    pub price_output: Option<Option<f64>>,
    pub price_cache_read: Option<Option<f64>>,
    pub price_cache_write: Option<Option<f64>>,
    pub price_input_audio: Option<Option<f64>>,
    pub price_output_audio: Option<Option<f64>>,
    pub price_reasoning: Option<Option<f64>>,
    pub limit_context: Option<Option<i64>>,
    pub limit_input: Option<Option<i64>>,
    pub limit_output: Option<Option<i64>>,
    pub modalities_input: Option<Value>,
    pub modalities_output: Option<Value>,
    pub release_date: Option<Option<i64>>,
    pub last_updated: Option<Option<i64>>,
    pub knowledge_cutoff: Option<Option<i64>>,
    pub open_weights: Option<Option<bool>>,
}

/// Syncs the system half of both tables from `catalog`, atomically.
///
/// An upsert keyed on each table's natural key — `(provider_id)` for a
/// provider, `(model_provider_id, model_id)` for a preset, both under
/// `user_id IS NULL` — rather than delete-then-insert: a row the directory
/// still publishes keeps its id across refreshes, which is what lets a model
/// point at a preset and keep pointing at it across a restart. A key the
/// directory stops publishing is left as it is; the directory does not retire
/// models, and an orphan row costs only a listing nobody reads. Only
/// `user_id IS NULL` rows are touched — a user's rows are left alone. The
/// caller decides what a failure means; the previous rows are left untouched
/// when this returns an error.
///
/// The id each row is written with is its identity on insert only: a row that
/// already exists keeps its id and its `created_at`, while every descriptive
/// field is overwritten from the catalog. Providers are upserted first and read
/// back, so the presets are keyed by the provider rows that already exist and a
/// system preset always points at a system provider.
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
            website: provider.website.clone(),
            api_base_url: provider.api_base_url.clone(),
        })
        .collect();

    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        async move {
            // Providers first: a preset's conflict key holds the provider row
            // id, so the upsert that leaves an existing row's id alone is also
            // what keeps every preset key stable across a refresh.
            let mut provider_ids: HashMap<String, Uuid> = HashMap::new();
            for chunk in new_providers.chunks(INSERT_CHUNK) {
                let written: Vec<(String, Uuid)> = diesel::insert_into(model_providers::table)
                    .values(chunk)
                    .on_conflict(model_providers::provider_id)
                    .filter_target(model_providers::user_id.is_null())
                    .do_update()
                    .set((
                        model_providers::name.eq(excluded(model_providers::name)),
                        model_providers::website.eq(excluded(model_providers::website)),
                        model_providers::api_base_url.eq(excluded(model_providers::api_base_url)),
                    ))
                    .returning((model_providers::provider_id, model_providers::id))
                    .load(conn)
                    .await?;
                provider_ids.extend(written);
            }

            let new_presets: Vec<NewModelPreset> = catalog
                .presets()
                .iter()
                .filter_map(|preset| {
                    let provider_id = *provider_ids.get(preset.provider.as_str())?;
                    Some(NewModelPreset::system(Uuid::now_v7(), provider_id, preset))
                })
                .collect();

            for chunk in new_presets.chunks(INSERT_CHUNK) {
                diesel::insert_into(model_presets::table)
                    .values(chunk)
                    .on_conflict((model_presets::model_provider_id, model_presets::model_id))
                    .filter_target(model_presets::user_id.is_null())
                    .do_update()
                    .set((
                        // Every descriptive column mirrors the catalog; the id,
                        // the owner, the conflict key, and `created_at` stay as
                        // they are — see the doc above.
                        model_presets::name.eq(excluded(model_presets::name)),
                        model_presets::vision.eq(excluded(model_presets::vision)),
                        model_presets::attachment.eq(excluded(model_presets::attachment)),
                        model_presets::reasoning.eq(excluded(model_presets::reasoning)),
                        model_presets::tools.eq(excluded(model_presets::tools)),
                        model_presets::structured_output
                            .eq(excluded(model_presets::structured_output)),
                        model_presets::temperature.eq(excluded(model_presets::temperature)),
                        model_presets::price_input.eq(excluded(model_presets::price_input)),
                        model_presets::price_output.eq(excluded(model_presets::price_output)),
                        model_presets::price_cache_read
                            .eq(excluded(model_presets::price_cache_read)),
                        model_presets::price_cache_write
                            .eq(excluded(model_presets::price_cache_write)),
                        model_presets::price_input_audio
                            .eq(excluded(model_presets::price_input_audio)),
                        model_presets::price_output_audio
                            .eq(excluded(model_presets::price_output_audio)),
                        model_presets::price_reasoning.eq(excluded(model_presets::price_reasoning)),
                        model_presets::limit_context.eq(excluded(model_presets::limit_context)),
                        model_presets::limit_input.eq(excluded(model_presets::limit_input)),
                        model_presets::limit_output.eq(excluded(model_presets::limit_output)),
                        model_presets::modalities_input
                            .eq(excluded(model_presets::modalities_input)),
                        model_presets::modalities_output
                            .eq(excluded(model_presets::modalities_output)),
                        model_presets::release_date.eq(excluded(model_presets::release_date)),
                        model_presets::last_updated.eq(excluded(model_presets::last_updated)),
                        model_presets::knowledge_cutoff
                            .eq(excluded(model_presets::knowledge_cutoff)),
                        model_presets::open_weights.eq(excluded(model_presets::open_weights)),
                    ))
                    .execute(conn)
                    .await?;
            }
            Ok(())
        }
        .scope_boxed()
    })
    .await
}

/// A token limit as the table stores it. The directory states limits as
/// unsigned counts; a value that somehow exceeds `i64` is dropped rather than
/// wrapped, since the column is `bigint`.
pub fn limit_to_db(limit: Option<u64>) -> Option<i64> {
    limit.and_then(|tokens| i64::try_from(tokens).ok())
}

fn limit_from_db(limit: Option<i64>) -> Option<u64> {
    limit.and_then(|tokens| u64::try_from(tokens).ok())
}

pub fn modalities_to_db(modalities: &[String]) -> Value {
    Value::Array(modalities.iter().cloned().map(Value::String).collect())
}

fn modalities_from_db(value: &Value) -> Vec<String> {
    serde_json::from_value(value.clone()).unwrap_or_default()
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
            name: "Claude Opus 5".into(),
            vision: true,
            attachment: true,
            reasoning: true,
            tools: true,
            structured_output: true,
            temperature: false,
            price_input: Some(3.0),
            price_output: Some(15.0),
            price_cache_read: Some(0.3),
            price_cache_write: None,
            price_input_audio: None,
            price_output_audio: None,
            price_reasoning: None,
            limit_context: Some(200_000),
            limit_input: None,
            limit_output: Some(64_000),
            modalities_input: serde_json::json!(["text", "image"]),
            modalities_output: serde_json::json!(["text"]),
            release_date: Some(1_729_036_800),
            last_updated: None,
            knowledge_cutoff: None,
            open_weights: Some(false),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_row_reads_back_as_the_preset_it_stored() {
        let preset = row().to_preset("anthropic", "Anthropic");

        assert_eq!(preset.provider, "anthropic");
        assert_eq!(preset.provider_name, "Anthropic");
        assert_eq!(preset.id, "claude-opus-5");
        assert_eq!(preset.name, "Claude Opus 5");
        assert!(preset.capabilities.vision);
        assert!(preset.capabilities.reasoning);
        assert!(!preset.capabilities.temperature);
        assert_eq!(preset.pricing.input, Some(3.0));
        assert_eq!(preset.pricing.cache_write, None);
        assert_eq!(preset.limits.context, Some(200_000));
        assert_eq!(preset.limits.input, None);
        assert_eq!(preset.limits.output, Some(64_000));
        assert_eq!(preset.modalities.input, vec!["text", "image"]);
        assert_eq!(preset.modalities.output, vec!["text"]);
        assert_eq!(preset.release_date, Some(1_729_036_800));
        assert_eq!(preset.open_weights, Some(false));
    }

    #[test]
    fn the_insert_shape_round_trips_through_the_read_shape() {
        let provider_id = Uuid::now_v7();
        let id = Uuid::now_v7();
        let original = row().to_preset("anthropic", "Anthropic");

        let new = NewModelPreset::owned(id, Uuid::nil(), provider_id, &original);

        assert_eq!(new.id, id);
        assert_eq!(new.user_id, Some(Uuid::nil()));
        assert_eq!(new.model_provider_id, provider_id);
        assert_eq!(new.model_id, original.id);
        assert_eq!(new.limit_context, original.limits.context.map(|v| v as i64));
        assert_eq!(new.modalities_input, serde_json::json!(["text", "image"]));
    }

    #[test]
    fn a_system_preset_has_no_owner() {
        let new =
            NewModelPreset::system(Uuid::now_v7(), Uuid::now_v7(), &row().to_preset("a", "A"));
        assert_eq!(new.user_id, None);
    }

    #[test]
    fn an_unparseable_modalities_blob_reads_as_empty_not_a_failure() {
        assert!(modalities_from_db(&serde_json::json!("text")).is_empty());
        assert!(modalities_from_db(&serde_json::json!(null)).is_empty());
    }
}
