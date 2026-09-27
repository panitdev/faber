//! The published catalog, as it is written on the wire.
//!
//! These types mirror the JSON at [`crate::DEFAULT_URL`] and nothing else:
//! they exist to be deserialized into and are never handed to a caller. The
//! public shape is [`crate::Catalog`], which splits the file into the units a
//! caller stores and answers the questions a caller actually asks.
//!
//! The file has two halves. `providers` is every serving provider with the
//! models it serves, each entry already *merged* with the model it serves:
//! upstream writes a provider's entry as a `base_model` reference plus
//! overrides, and the generator inlines the base and drops the reference.
//! `models` is the provider-agnostic half — one entry per model as its creator
//! describes it, keyed `<creator>/<model>`.
//!
//! Almost every field is optional here even where upstream's schema requires
//! it. The file is a third party's, regenerated on its own schedule; a model
//! missing a date or a limit is the normal case, not a parse failure that
//! should take the whole catalog down.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireCatalog {
    /// Serving provider key → provider.
    pub providers: BTreeMap<String, WireProvider>,
    /// `<creator>/<model>` → the model as its creator describes it.
    pub models: BTreeMap<String, WireModel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireProvider {
    pub id: String,
    pub name: Option<String>,
    /// The AI SDK package that speaks this provider's API.
    pub npm: Option<String>,
    /// Environment variable names upstream reads the provider's key from.
    pub env: Vec<String>,
    pub doc: Option<String>,
    /// An OpenAI-compatible endpoint, stated when `npm` is the generic
    /// compatible package and so names no endpoint of its own.
    pub api: Option<String>,
    pub models: BTreeMap<String, WireModel>,
}

/// One model entry. The same shape serves both halves of the file: a creator
/// model never carries `cost` or the other serving-only fields, and a serving
/// entry never carries `license`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireModel {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub family: Option<String>,
    pub attachment: bool,
    pub reasoning: bool,
    pub tool_call: bool,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    /// `YYYY-MM` or `YYYY-MM-DD`.
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub open_weights: Option<bool>,
    pub limit: Option<WireLimit>,
    pub modalities: Option<WireModalities>,
    pub license: Option<String>,
    pub cost: Option<WireCost>,
    pub reasoning_options: Option<Value>,
    pub interleaved: Option<Value>,
    pub status: Option<String>,
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

/// US dollars per million tokens. Ints and floats both occur in the file, so
/// every price is `f64`. Anything else — tiered prices, a long-context rate —
/// is kept as upstream wrote it rather than modelled here.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireCost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub input_audio: Option<f64>,
    pub output_audio: Option<f64>,
    pub reasoning: Option<f64>,
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}
