//! The published directory, as it is written on the wire.
//!
//! These types mirror the JSON at [`crate::DEFAULT_URL`] and nothing else:
//! they exist to be deserialized into and are never handed to a caller. The
//! public shape is [`crate::Catalog`], which drops the fields this service has
//! no use for and answers the questions a caller actually asks.
//!
//! Every field is optional. The directory is a third party's file regenerated
//! on its own schedule; a model missing a price or a modality is the normal
//! case, not a parse failure that should take the whole catalog down.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// Top-level map: provider key → provider. The key and the provider's own
/// `id` are the same string in the current file; the key is what we index by
/// so a future divergence cannot lose a provider.
pub(crate) type Directory = BTreeMap<String, WireProvider>;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireProvider {
    pub id: String,
    pub name: Option<String>,
    pub website: Option<String>,
    #[serde(rename = "apiBaseUrl")]
    pub api_base_url: Option<String>,
    pub models: BTreeMap<String, WireModel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireModel {
    pub id: String,
    pub name: Option<String>,
    /// Epoch seconds, usually a string and occasionally the number `0`.
    pub release_date: Option<Value>,
    pub last_updated: Option<Value>,
    pub knowledge_cutoff: Option<Value>,
    pub open_weights: Option<bool>,
    pub features: WireFeatures,
    pub pricing: WirePricing,
    pub limit: WireLimit,
    pub modalities: WireModalities,
}

/// What the model can do. Absent means "the directory says nothing", which
/// every consumer here reads as `false`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireFeatures {
    pub attachment: bool,
    pub reasoning: bool,
    pub tool_call: bool,
    pub structured_output: bool,
    pub temperature: bool,
}

/// US dollars per million tokens. Ints and floats both occur in the file, so
/// every field is `f64`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WirePricing {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub input_audio: Option<f64>,
    pub output_audio: Option<f64>,
    pub reasoning: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireLimit {
    pub context: Option<u64>,
    pub input: Option<u64>,
    pub output: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireModalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}
