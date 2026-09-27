//! Creator models: a model as the lab that made it describes it.
//!
//! One row per model, keyed `<creator>/<model>` — what it can do, its context
//! window, its modalities, its release — independent of who serves it. A
//! [`crate::models::model_preset`] row is one provider serving a model and
//! links here, storing only what it says differently. That link is what keeps
//! a model served by twenty providers from being described twenty times.
//!
//! Every row is the system's: the catalog upserts them at boot and never
//! deletes one, so a preset — the system's or a user's — can link to any of
//! them and keep the link. There is no owner column and no write route.

use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::creator_models;

/// One creator model as stored.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = creator_models)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CreatorModelRow {
    pub id: Uuid,
    /// `<creator>/<model>`, e.g. `anthropic/claude-haiku-4-5`.
    pub model_id: String,
    pub creator: String,
    pub name: String,
    pub description: Option<String>,
    pub family: Option<String>,
    pub attachment: bool,
    pub reasoning: bool,
    pub tool_call: bool,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub open_weights: Option<bool>,
    pub limits: Value,
    pub modalities: Value,
    pub license: Option<String>,
}

impl CreatorModelRow {
    pub fn to_spec(&self) -> presets::Spec {
        presets::Spec {
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
            limit: from_db(&self.limits),
            modalities: from_db(&self.modalities),
        }
    }

    pub fn to_creator_model(&self) -> presets::CreatorModel {
        presets::CreatorModel {
            id: self.model_id.clone(),
            creator: self.creator.clone(),
            spec: self.to_spec(),
            license: self.license.clone(),
        }
    }
}

/// One creator model as it is written. The id names the row only when it is
/// inserted — the catalog load upserts, and a row that already exists keeps
/// its own.
#[derive(Debug, Insertable)]
#[diesel(table_name = creator_models)]
pub struct NewCreatorModel {
    pub id: Uuid,
    pub model_id: String,
    pub creator: String,
    pub name: String,
    pub description: Option<String>,
    pub family: Option<String>,
    pub attachment: bool,
    pub reasoning: bool,
    pub tool_call: bool,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub open_weights: Option<bool>,
    pub limits: Value,
    pub modalities: Value,
    pub license: Option<String>,
}

impl NewCreatorModel {
    pub fn new(id: Uuid, model: &presets::CreatorModel) -> Self {
        let spec = &model.spec;
        Self {
            id,
            model_id: model.id.clone(),
            creator: model.creator.clone(),
            name: spec.name.clone(),
            description: spec.description.clone(),
            family: spec.family.clone(),
            attachment: spec.attachment,
            reasoning: spec.reasoning,
            tool_call: spec.tool_call,
            structured_output: spec.structured_output,
            temperature: spec.temperature,
            knowledge: spec.knowledge.clone(),
            release_date: spec.release_date.clone(),
            last_updated: spec.last_updated.clone(),
            open_weights: spec.open_weights,
            limits: to_db(&spec.limit),
            modalities: to_db(&spec.modalities),
            license: model.license.clone(),
        }
    }
}

/// A JSON column as the value it stores. A blob that does not parse reads as
/// the default rather than failing the row: a limit nobody can read is a limit
/// nobody stated.
pub(crate) fn from_db<T: serde::de::DeserializeOwned + Default>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

pub(crate) fn to_db<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> presets::CreatorModel {
        presets::CreatorModel {
            id: "anthropic/claude-opus-5".into(),
            creator: "anthropic".into(),
            spec: presets::Spec {
                name: "Claude Opus 5".into(),
                description: Some("Frontier model".into()),
                reasoning: true,
                tool_call: true,
                release_date: Some("2026-03-01".into()),
                limit: presets::Limit {
                    context: Some(1_000_000),
                    input: None,
                    output: Some(128_000),
                },
                modalities: presets::Modalities {
                    input: vec!["text".into(), "image".into()],
                    output: vec!["text".into()],
                },
                ..Default::default()
            },
            license: None,
        }
    }

    #[test]
    fn a_creator_model_round_trips_through_its_row() {
        let new = NewCreatorModel::new(Uuid::nil(), &model());
        let row = CreatorModelRow {
            id: new.id,
            model_id: new.model_id,
            creator: new.creator,
            name: new.name,
            description: new.description,
            family: new.family,
            attachment: new.attachment,
            reasoning: new.reasoning,
            tool_call: new.tool_call,
            structured_output: new.structured_output,
            temperature: new.temperature,
            knowledge: new.knowledge,
            release_date: new.release_date,
            last_updated: new.last_updated,
            open_weights: new.open_weights,
            limits: new.limits,
            modalities: new.modalities,
            license: new.license,
        };
        assert_eq!(row.to_creator_model(), model());
    }

    #[test]
    fn an_unparseable_blob_reads_as_the_default_not_a_failure() {
        let limit: presets::Limit = from_db(&serde_json::json!("lots"));
        assert_eq!(limit, presets::Limit::default());
        let modalities: presets::Modalities = from_db(&serde_json::json!(null));
        assert!(modalities.input.is_empty());
    }
}
