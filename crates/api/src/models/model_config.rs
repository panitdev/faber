use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{models::thinking::ThinkingCapability, schema::models};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    Openai,
    Anthropic,
}

impl Wire {
    pub fn as_str(&self) -> &'static str {
        match self {
            Wire::Openai => "openai",
            Wire::Anthropic => "anthropic",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(Wire::Openai),
            "anthropic" => Some(Wire::Anthropic),
            _ => None,
        }
    }
}

/// A configured model: how a run reaches an endpoint, and which preset
/// describes what it is.
///
/// Everything descriptive — what the model can do, what it costs, its context
/// window — comes from the referenced preset, not from this row. The row
/// carries only what a request needs: the alias the user types, a `base_url`,
/// a `wire`, the provider's own `wire_id`, the credential, and the per-endpoint
/// tuning in `params`.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = models)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ModelConfig {
    pub id: Uuid,
    pub user_id: Uuid,
    pub alias: String,
    pub base_url: String,
    pub wire: String,
    pub wire_id: String,
    pub family: Option<String>,
    pub credential_id: Option<Uuid>,
    pub params: Value,
    pub preset_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// The key under `params` that carries the reasoning-history policy.
pub(crate) const REASONING_HISTORY_KEY: &str = "reasoning_history";

/// The key under `params` that carries the thinking knob's definition —
/// see [`ThinkingCapability`].
pub(crate) const THINKING_KEY: &str = "thinking";

/// The key under `params` that carries per-endpoint request tweaks — see
/// [`llm::AdvancedOptions`].
pub(crate) const ADVANCED_KEY: &str = "advanced";

impl ModelConfig {
    /// How much of a replayed assistant turn's reasoning this model wants back.
    ///
    /// `None` means the row says nothing and the wire's own default applies —
    /// which is also the answer for a row written before this setting existed.
    /// The value is validated when the row is written (see `routes::models`),
    /// so a row this can't parse went around the API, and [`ModelParams`]'s own
    /// field falls back to `None` for exactly that case rather than taking the
    /// rest of the row down with it — see [`lenient`].
    pub fn reasoning_history(&self) -> Option<llm::ReasoningHistory> {
        self.params().reasoning_history
    }

    /// Whether this model reasons, and at which levels — what the session's
    /// thinking knob is read against.
    ///
    /// Degrades the same way [`Self::reasoning_history`] does, and to the same
    /// end: a `params.thinking` the write path would have rejected leaves this
    /// model without a thinking knob rather than taking the run down with it.
    pub fn thinking(&self) -> ThinkingCapability {
        self.params().thinking
    }

    /// Per-endpoint request tweaks this model wants applied to every call.
    ///
    /// Defaults to the no-op value when the row says nothing, and — like
    /// [`Self::reasoning_history`] — when it says something the write path
    /// would have rejected: a row that went around the API should degrade, not
    /// fail the run.
    pub fn advanced_options(&self) -> llm::AdvancedOptions {
        self.params().advanced
    }

    /// The typed view of the free-form `params` column. A blob that does not
    /// parse at all yields the default; each field's own leniency (see
    /// [`lenient`]) keeps one bad key from taking the others down with it.
    fn params(&self) -> ModelParams {
        serde_json::from_value(self.params.clone()).unwrap_or_default()
    }
}

/// Deserializes `T`, defaulting instead of failing the surrounding struct's
/// parse when this one field doesn't fit. Used only on the fields whose
/// write path validates them strictly (see `parse_reasoning_history`,
/// `parse_thinking_capability`, `parse_advanced_options`) — a row that went
/// around that validation should degrade on that field alone, not drag every
/// other field on the row down with it.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Reads a `params.reasoning_history` value, naming what is wrong with it
/// rather than falling back — the write path wants the complaint.
pub fn parse_reasoning_history(value: &Value) -> Result<Option<llm::ReasoningHistory>, String> {
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| {
            format!("{REASONING_HISTORY_KEY} must be one of \"full\", \"text\", or \"omitted\"")
        })
}

/// Reads a `params.advanced` value, naming what is wrong with it rather than
/// falling back — the write path wants the complaint.
pub fn parse_advanced_options(value: &Value) -> Result<llm::AdvancedOptions, String> {
    if value.is_null() {
        return Ok(llm::AdvancedOptions::default());
    }
    serde_json::from_value(value.clone()).map_err(|_| {
        format!(
            "{ADVANCED_KEY} must be an object with an optional \"reasoning_split\" boolean and \
             an optional \"extra\" object"
        )
    })
}

#[derive(Insertable)]
#[diesel(table_name = models)]
pub struct NewModelConfig<'a> {
    pub id: Uuid,
    pub user_id: Uuid,
    pub alias: &'a str,
    pub base_url: &'a str,
    pub wire: &'a str,
    pub wire_id: &'a str,
    pub family: Option<&'a str>,
    pub credential_id: Option<Uuid>,
    pub params: Value,
    pub preset_id: Option<Uuid>,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = models)]
pub struct UpdateModelConfig<'a> {
    pub alias: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub wire: Option<&'a str>,
    pub wire_id: Option<&'a str>,
    pub family: Option<Option<&'a str>>,
    pub credential_id: Option<Option<Uuid>>,
    pub params: Option<Value>,
    pub preset_id: Option<Option<Uuid>>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ModelParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    /// See [`ModelConfig::reasoning_history`] — that reader is the one the run
    /// path uses, since it has to answer for a row that carries nothing here.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_history: Option<llm::ReasoningHistory>,
    /// See [`ModelConfig::thinking`] — lenient here for the same reason
    /// `reasoning_history` is: its write path validates it strictly.
    #[serde(default, deserialize_with = "lenient")]
    pub thinking: ThinkingCapability,
    /// See [`ModelConfig::advanced_options`] — that reader is the one the run
    /// path uses, since it has to answer for a row that carries nothing here.
    #[serde(default, deserialize_with = "lenient")]
    pub advanced: llm::AdvancedOptions,
    #[serde(flatten)]
    pub passthrough: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(params: Value) -> ModelConfig {
        ModelConfig {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            alias: "fast".into(),
            base_url: "https://api.anthropic.com".into(),
            wire: "anthropic".into(),
            wire_id: "claude-opus-5".into(),
            family: None,
            credential_id: None,
            params,
            preset_id: None,
            created_at: Utc::now(),
        }
    }

    /// The parsed form of a `params` blob, the way the readers above see it —
    /// `ModelConfig` keeps the raw `Value`.
    fn params(value: Value) -> ModelParams {
        serde_json::from_value(value).expect("params always parse")
    }

    #[test]
    fn a_row_that_says_nothing_leaves_the_wire_its_default() {
        assert!(config(json!({})).reasoning_history().is_none());
        assert!(config(json!(null)).reasoning_history().is_none());
    }

    #[test]
    fn each_setting_reaches_the_run_as_itself() {
        for (written, expected) in [
            ("full", llm::ReasoningHistory::Full),
            ("text", llm::ReasoningHistory::Text),
            ("omitted", llm::ReasoningHistory::Omitted),
        ] {
            let config = config(json!({ REASONING_HISTORY_KEY: written }));
            assert_eq!(config.reasoning_history(), Some(expected));
        }
    }

    #[test]
    fn a_row_written_around_the_api_falls_back_rather_than_failing_the_run() {
        // The write path refuses this value; a row carrying it anyway came
        // from somewhere else, and the wire default beats refusing to run.
        let config = config(json!({ REASONING_HISTORY_KEY: "sometimes" }));
        assert!(config.reasoning_history().is_none());
    }

    #[test]
    fn the_write_path_gets_told_what_is_wrong_with_the_value() {
        assert!(parse_reasoning_history(&json!("sometimes")).is_err());
        assert!(parse_reasoning_history(&json!(true)).is_err());
        assert_eq!(parse_reasoning_history(&json!(null)), Ok(None));
    }

    #[test]
    fn other_params_are_left_alone() {
        let config = config(json!({"temperature": 0.5, REASONING_HISTORY_KEY: "text"}));
        assert_eq!(
            config.reasoning_history(),
            Some(llm::ReasoningHistory::Text)
        );
        assert_eq!(config.params["temperature"], json!(0.5));
    }

    #[test]
    fn a_row_that_says_nothing_has_no_thinking_knob() {
        assert_eq!(config(json!({})).thinking(), ThinkingCapability::default());
        assert!(!config(json!({})).thinking().supported);
    }

    #[test]
    fn the_thinking_knob_reaches_the_run_as_written() {
        let config = config(json!({
            THINKING_KEY: { "supported": true, "efforts": ["low", "high"], "default_effort": "high" }
        }));
        assert_eq!(
            config.thinking(),
            ThinkingCapability {
                supported: true,
                efforts: vec![llm::Effort::Low, llm::Effort::High],
                default_effort: Some(llm::Effort::High),
            }
        );
    }

    #[test]
    fn a_thinking_knob_written_around_the_api_leaves_the_model_without_one() {
        // The write path refuses this; a row carrying it anyway came from
        // somewhere else, and no knob beats failing the run.
        assert_eq!(
            config(json!({ THINKING_KEY: "sometimes" })).thinking(),
            ThinkingCapability::default()
        );
        // And it does not take the rest of the row down with it.
        let mixed = config(json!({ THINKING_KEY: "sometimes", REASONING_HISTORY_KEY: "text" }));
        assert_eq!(mixed.reasoning_history(), Some(llm::ReasoningHistory::Text));
    }

    #[test]
    fn a_row_that_says_nothing_gets_the_no_op_advanced_options() {
        assert_eq!(
            config(json!({})).advanced_options(),
            llm::AdvancedOptions::default()
        );
    }

    #[test]
    fn advanced_options_reach_the_run_as_written() {
        let config =
            config(json!({ ADVANCED_KEY: { "reasoning_split": true, "extra": { "top_k": 5 } } }));
        assert_eq!(
            config.advanced_options(),
            llm::AdvancedOptions {
                reasoning_split: true,
                extra: serde_json::json!({ "top_k": 5 })
                    .as_object()
                    .unwrap()
                    .clone(),
            }
        );
    }

    #[test]
    fn a_row_written_around_the_api_falls_back_to_the_no_op_value() {
        // The write path refuses a non-object `advanced`; a row carrying one
        // anyway came from somewhere else, and a no-op beats failing the run.
        let config = config(json!({ ADVANCED_KEY: "yes please" }));
        assert_eq!(config.advanced_options(), llm::AdvancedOptions::default());
    }

    #[test]
    fn the_write_path_gets_told_what_is_wrong_with_advanced_options() {
        assert!(parse_advanced_options(&json!("yes please")).is_err());
        assert!(parse_advanced_options(&json!({ "reasoning_split": "yes" })).is_err());
        assert_eq!(
            parse_advanced_options(&json!(null)),
            Ok(llm::AdvancedOptions::default())
        );
    }

    #[test]
    fn other_params_are_left_alone_by_the_advanced_reader() {
        let config = config(json!({"temperature": 0.5, ADVANCED_KEY: { "reasoning_split": true }}));
        assert!(config.advanced_options().reasoning_split);
        assert_eq!(config.params["temperature"], json!(0.5));
    }

    #[test]
    fn a_non_object_params_blob_yields_every_default() {
        let config = config(json!("not an object"));
        assert!(config.reasoning_history().is_none());
        assert_eq!(config.thinking(), ThinkingCapability::default());
        assert_eq!(config.advanced_options(), llm::AdvancedOptions::default());
    }

    #[test]
    fn one_bad_key_does_not_take_the_others_down() {
        let parsed = params(json!({
            THINKING_KEY: "sometimes",
            REASONING_HISTORY_KEY: "text",
            ADVANCED_KEY: { "reasoning_split": true }
        }));
        assert_eq!(parsed.thinking, ThinkingCapability::default());
        assert_eq!(parsed.reasoning_history, Some(llm::ReasoningHistory::Text));
        assert!(parsed.advanced.reasoning_split);
    }
}
