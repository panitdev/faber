//! The read-only catalog: what the directory publishes, shaped for callers.
//!
//! The catalog comes in three units, the way upstream itself thinks of it:
//!
//! - a [`Provider`] is somebody who serves models over an API;
//! - a [`CreatorModel`] is a model as the lab that made it describes it —
//!   what it can do, its context window, its release — independent of who
//!   serves it;
//! - an [`Offering`] is one provider serving one model: what it costs there,
//!   and whatever that provider says differently from the creator.
//!
//! An offering stores only its difference from the creator model it is linked
//! to, as [`Overrides`]. That is what keeps the same description, dates, and
//! limits from being copied onto every provider that serves a model. An
//! offering with no link carries every field itself, and the one resolution
//! path — [`Overrides::apply`] — reads both the same way.
//!
//! None of this is a configured model and none of it can become one by
//! accident — nothing here is persisted, and nothing carries a `base_url` a
//! request is sent to.
//!
//! Serialization is part of the public contract: the catalog is already a
//! projection of somebody else's JSON, and a caller that exposes it (the
//! `api` crate's browse routes) needs the same shape this crate already
//! settled on. The field names are upstream's, so a reader who knows the
//! catalog knows the API.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, wire};

/// Every provider, creator model, and offering the directory publishes.
#[derive(Debug)]
pub struct Catalog {
    providers: Vec<Provider>,
    models: Vec<CreatorModel>,
    offerings: Vec<Offering>,
    /// Creator model id → index into `models`.
    model_index: HashMap<String, usize>,
    /// `(provider, served id)` → index into `offerings`.
    offering_index: HashMap<(String, String), usize>,
}

impl Catalog {
    /// Reads the directory from its bytes. Pure — no network, no state.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let catalog: wire::WireCatalog = serde_json::from_slice(bytes)?;
        Ok(Self::build(catalog))
    }

    fn build(catalog: wire::WireCatalog) -> Self {
        let mut models = Vec::with_capacity(catalog.models.len());
        let mut model_index = HashMap::new();
        for (key, model) in catalog.models {
            let id = non_empty(model.id.clone()).unwrap_or(key);
            // The id is `<creator>/<model>`; the prefix is the only thing the
            // file says about the creator, so it is all a creator is here.
            let creator = id.split_once('/').map_or(id.as_str(), |(creator, _)| creator);
            let creator = creator.to_owned();
            let license = model.license.clone();
            model_index.insert(id.clone(), models.len());
            models.push(CreatorModel {
                spec: spec(&id, &model),
                id,
                creator,
                license,
            });
        }

        let linker = Linker::new(&models, &model_index);

        let mut providers = Vec::with_capacity(catalog.providers.len());
        let mut offerings = Vec::new();
        let mut offering_index = HashMap::new();

        for (key, provider) in catalog.providers {
            // The key and the provider's own `id` agree today; preferring the
            // id keeps the lookup key stable if the map key is ever renamed.
            let provider_id = non_empty(provider.id).unwrap_or(key);
            let provider_name = provider
                .name
                .and_then(non_empty)
                .unwrap_or_else(|| provider_id.clone());

            let model_count = provider.models.len();
            for (map_key, model) in provider.models {
                let id = non_empty(model.id.clone()).unwrap_or(map_key);
                let merged = spec(&id, &model);

                // A link is kept only when the difference can be written down:
                // an offering that drops a field its base states has no
                // override that says "absent", and linking it anyway would
                // resolve to the base's value rather than the file's.
                let linked = linker.link(&provider_id, &id, &merged).and_then(|base| {
                    let overrides = Overrides::diff(&merged, &models[base].spec)?;
                    Some((models[base].id.clone(), overrides))
                });
                let (base_model, overrides) = match linked {
                    Some((base, overrides)) => (Some(base), overrides),
                    None => (None, Overrides::full(&merged)),
                };

                offering_index.insert((provider_id.clone(), id.clone()), offerings.len());
                offerings.push(Offering {
                    provider: provider_id.clone(),
                    id,
                    base_model,
                    overrides,
                    serving: serving(model),
                });
            }

            providers.push(Provider {
                id: provider_id,
                name: provider_name,
                npm: provider.npm.and_then(non_empty),
                env: provider.env,
                doc: provider.doc.and_then(non_empty),
                api: provider.api.and_then(non_empty),
                model_count,
            });
        }

        Self {
            providers,
            models,
            offerings,
            model_index,
            offering_index,
        }
    }

    /// Providers in the directory's alphabetical order.
    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    /// Creator models, alphabetical by `<creator>/<model>`.
    pub fn models(&self) -> &[CreatorModel] {
        &self.models
    }

    /// Every offering, providers and their models both alphabetically ordered.
    pub fn offerings(&self) -> &[Offering] {
        &self.offerings
    }

    /// The number of offerings — what a caller can bind a model to.
    pub fn len(&self) -> usize {
        self.offerings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offerings.is_empty()
    }

    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// One creator model, by its `<creator>/<model>` id.
    pub fn model(&self, id: &str) -> Option<&CreatorModel> {
        self.model_index.get(id).map(|&index| &self.models[index])
    }

    /// One offering, by the provider that serves it and the id it serves it
    /// under.
    pub fn offering(&self, provider: &str, id: &str) -> Option<&Offering> {
        self.offering_index
            .get(&(provider.to_owned(), id.to_owned()))
            .map(|&index| &self.offerings[index])
    }

    /// An offering as the file stated it: its overrides laid over the creator
    /// model it is linked to, when it is linked to one.
    pub fn resolve(&self, offering: &Offering) -> Spec {
        let base = offering
            .base_model
            .as_deref()
            .and_then(|id| self.model(id))
            .map(|model| &model.spec);
        offering.overrides.apply(base, &offering.id)
    }
}

impl std::str::FromStr for Catalog {
    type Err = Error;

    /// The same as [`Catalog::parse`], from a JSON string.
    fn from_str(json: &str) -> Result<Self, Self::Err> {
        Self::parse(json.as_bytes())
    }
}

/// Finds the creator model a serving entry was generated from.
///
/// The file carries no link — upstream's `base_model` is inlined and dropped —
/// so it is recovered, strongest evidence first:
///
/// 1. the served id *is* a creator model id, as it is on routers that serve
///    under `<creator>/<model>`;
/// 2. the provider is the creator and serves the model under its bare name,
///    as `anthropic` serves `anthropic/claude-haiku-4-5` as
///    `claude-haiku-4-5`;
/// 3. the entry inherited its description verbatim, and its release date and
///    family agree. Description alone is not enough — a provider that writes
///    a generic blurb can collide with an unrelated model's — so the dates
///    and family must match too, and a description two creator models share
///    identifies neither.
///
/// A wrong link costs little: the offering stores every field that differs
/// from the linked model, so it still resolves to exactly what the file said.
/// What a wrong link would misstate is which model this is, which is why the
/// third rule is as strict as it is.
struct Linker<'a> {
    models: &'a [CreatorModel],
    index: &'a HashMap<String, usize>,
    /// Description → the one creator model that has it; `None` when several do.
    by_description: HashMap<&'a str, Option<usize>>,
}

impl<'a> Linker<'a> {
    fn new(models: &'a [CreatorModel], index: &'a HashMap<String, usize>) -> Self {
        let mut by_description: HashMap<&str, Option<usize>> = HashMap::new();
        for (position, model) in models.iter().enumerate() {
            let Some(description) = model.spec.description.as_deref() else {
                continue;
            };
            by_description
                .entry(description)
                .and_modify(|slot| *slot = None)
                .or_insert(Some(position));
        }
        Self {
            models,
            index,
            by_description,
        }
    }

    fn link(&self, provider: &str, id: &str, spec: &Spec) -> Option<usize> {
        if let Some(&position) = self.index.get(id) {
            return Some(position);
        }
        if let Some(&position) = self.index.get(&format!("{provider}/{id}")) {
            return Some(position);
        }
        let position = (*self.by_description.get(spec.description.as_deref()?)?)?;
        let base = &self.models[position].spec;
        (base.release_date == spec.release_date && base.family == spec.family).then_some(position)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// The provider's key, e.g. `anthropic`.
    pub id: String,
    pub name: String,
    /// The AI SDK package that speaks this provider's API, e.g.
    /// `@ai-sdk/anthropic`.
    pub npm: Option<String>,
    /// Environment variable names upstream reads the key from. Informational:
    /// this service never reads a provider key from its own environment.
    pub env: Vec<String>,
    /// The provider's model documentation.
    pub doc: Option<String>,
    /// An OpenAI-compatible endpoint, when the provider states one.
    /// Informational: a preset never routes a request.
    pub api: Option<String>,
    pub model_count: usize,
}

/// A model as its creator describes it, independent of who serves it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreatorModel {
    /// `<creator>/<model>`, e.g. `anthropic/claude-haiku-4-5`.
    pub id: String,
    /// The lab that made it, e.g. `anthropic`.
    pub creator: String,
    #[serde(flatten)]
    pub spec: Spec,
    /// Only a creator states a license; a provider serving the model does not.
    pub license: Option<String>,
}

/// One provider serving one model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offering {
    pub provider: String,
    /// The model id as served, e.g. `claude-opus-5`.
    pub id: String,
    /// The creator model this offering is linked to — upstream's name for the
    /// same reference. `None` for a model no creator entry describes.
    pub base_model: Option<String>,
    pub overrides: Overrides,
    #[serde(flatten)]
    pub serving: Serving,
}

/// What a model is and can do. Held whole by a [`CreatorModel`], and produced
/// for an offering by laying its [`Overrides`] over one.
///
/// [`Default`] states nothing: every capability absent, no limits, no
/// modalities, no dates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    pub name: String,
    pub description: Option<String>,
    /// A coarse grouping, e.g. `claude-haiku`. Not an identity.
    pub family: Option<String>,
    pub attachment: bool,
    pub reasoning: bool,
    pub tool_call: bool,
    pub structured_output: Option<bool>,
    /// Whether the endpoint takes a temperature at all.
    pub temperature: Option<bool>,
    /// Knowledge cutoff, `YYYY-MM` or `YYYY-MM-DD` as upstream writes it.
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub open_weights: Option<bool>,
    pub limit: Limit,
    pub modalities: Modalities,
}

impl Spec {
    /// Image input. Upstream states it as a modality rather than a flag.
    pub fn vision(&self) -> bool {
        self.modalities
            .input
            .iter()
            .any(|modality| modality.eq_ignore_ascii_case("image"))
    }
}

/// How an offering differs from the creator model it is linked to. `None` is
/// "as the base says"; an unlinked offering sets every field it knows.
///
/// `limit` and `modalities` are whole units, not per-key: upstream overrides
/// them whole, and a provider stating only its output limit does not inherit
/// the creator's input limit by accident.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
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
    pub limit: Option<Limit>,
    pub modalities: Option<Modalities>,
}

impl Overrides {
    /// Every field of `spec`, for an offering that stands on its own.
    pub fn full(spec: &Spec) -> Self {
        Self {
            name: Some(spec.name.clone()),
            description: spec.description.clone(),
            family: spec.family.clone(),
            attachment: Some(spec.attachment),
            reasoning: Some(spec.reasoning),
            tool_call: Some(spec.tool_call),
            structured_output: spec.structured_output,
            temperature: spec.temperature,
            knowledge: spec.knowledge.clone(),
            release_date: spec.release_date.clone(),
            last_updated: spec.last_updated.clone(),
            open_weights: spec.open_weights,
            limit: Some(spec.limit),
            modalities: Some(spec.modalities.clone()),
        }
    }

    /// Only what `spec` says differently from `base`.
    ///
    /// `None` when the difference cannot be written down: `spec` leaves out a
    /// field `base` states, and an override has no way to say "absent" — it
    /// can only fall through to the base.
    pub fn diff(spec: &Spec, base: &Spec) -> Option<Self> {
        fn scalar<T: Clone + PartialEq>(spec: &T, base: &T) -> Option<T> {
            (spec != base).then(|| spec.clone())
        }
        fn optional<T: Clone + PartialEq>(
            spec: &Option<T>,
            base: &Option<T>,
        ) -> Result<Option<T>, ()> {
            match (spec, base) {
                (None, Some(_)) => Err(()),
                _ => Ok(scalar(spec, base).flatten()),
            }
        }

        Some(Self {
            name: scalar(&spec.name, &base.name),
            description: optional(&spec.description, &base.description).ok()?,
            family: optional(&spec.family, &base.family).ok()?,
            attachment: scalar(&spec.attachment, &base.attachment),
            reasoning: scalar(&spec.reasoning, &base.reasoning),
            tool_call: scalar(&spec.tool_call, &base.tool_call),
            structured_output: optional(&spec.structured_output, &base.structured_output).ok()?,
            temperature: optional(&spec.temperature, &base.temperature).ok()?,
            knowledge: optional(&spec.knowledge, &base.knowledge).ok()?,
            release_date: optional(&spec.release_date, &base.release_date).ok()?,
            last_updated: optional(&spec.last_updated, &base.last_updated).ok()?,
            open_weights: optional(&spec.open_weights, &base.open_weights).ok()?,
            limit: scalar(&spec.limit, &base.limit),
            modalities: scalar(&spec.modalities, &base.modalities),
        })
    }

    /// The overrides laid over `base`. With no base, the empty [`Spec`] is the
    /// base, and a name nobody states falls back to `id`.
    pub fn apply(&self, base: Option<&Spec>, id: &str) -> Spec {
        let mut spec = base.cloned().unwrap_or_default();
        fn set<T: Clone>(slot: &mut T, value: &Option<T>) {
            if let Some(value) = value {
                *slot = value.clone();
            }
        }
        fn set_optional<T: Clone>(slot: &mut Option<T>, value: &Option<T>) {
            if value.is_some() {
                slot.clone_from(value);
            }
        }

        set(&mut spec.name, &self.name);
        set_optional(&mut spec.description, &self.description);
        set_optional(&mut spec.family, &self.family);
        set(&mut spec.attachment, &self.attachment);
        set(&mut spec.reasoning, &self.reasoning);
        set(&mut spec.tool_call, &self.tool_call);
        set_optional(&mut spec.structured_output, &self.structured_output);
        set_optional(&mut spec.temperature, &self.temperature);
        set_optional(&mut spec.knowledge, &self.knowledge);
        set_optional(&mut spec.release_date, &self.release_date);
        set_optional(&mut spec.last_updated, &self.last_updated);
        set_optional(&mut spec.open_weights, &self.open_weights);
        set(&mut spec.limit, &self.limit);
        set(&mut spec.modalities, &self.modalities);
        if spec.name.trim().is_empty() {
            spec.name = id.to_owned();
        }
        spec
    }
}

/// What only a serving provider can say about a model: the price, the
/// reasoning controls its API exposes, and its lifecycle.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Serving {
    pub cost: Option<Cost>,
    /// Upstream's list of reasoning controls, e.g.
    /// `[{"type": "effort", "values": ["low", "high"]}]`. Kept as written.
    pub reasoning_options: Option<Value>,
    /// Whether and how reasoning is interleaved with output. Kept as written.
    pub interleaved: Option<Value>,
    /// `alpha`, `beta`, or `deprecated`.
    pub status: Option<String>,
}

/// US dollars per million tokens. `None` is *unknown*, not free: a provider
/// that does not bill a component simply omits it. Tiers and long-context
/// rates stay in `other`, as upstream wrote them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cost {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limit {
    /// Total context window, in tokens.
    pub context: Option<u64>,
    /// Maximum prompt, where it is stated separately.
    pub input: Option<u64>,
    /// Maximum output, where it is stated.
    pub output: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Modalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

/// An offering as a caller shows it: the provider, the creator model it is
/// linked to, and the resolved description. [`Default`] is the built-in empty
/// preset — what a configured model with no preset is read against: a
/// description that says nothing and so works for any model, rather than an
/// error or a missing metadata blob.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    /// The serving provider's key, e.g. `anthropic`.
    pub provider: String,
    pub provider_name: String,
    /// The model id as served, e.g. `claude-opus-5`.
    pub id: String,
    /// The creator model this is linked to, e.g. `anthropic/claude-opus-5`.
    pub base_model: Option<String>,
    #[serde(flatten)]
    pub spec: Spec,
    #[serde(flatten)]
    pub serving: Serving,
}

fn spec(id: &str, model: &wire::WireModel) -> Spec {
    Spec {
        name: model
            .name
            .clone()
            .and_then(non_empty)
            .unwrap_or_else(|| id.to_owned()),
        description: model.description.clone().and_then(non_empty),
        family: model.family.clone().and_then(non_empty),
        attachment: model.attachment,
        reasoning: model.reasoning,
        tool_call: model.tool_call,
        structured_output: model.structured_output,
        temperature: model.temperature,
        knowledge: model.knowledge.clone().and_then(non_empty),
        release_date: model.release_date.clone().and_then(non_empty),
        last_updated: model.last_updated.clone().and_then(non_empty),
        open_weights: model.open_weights,
        limit: model
            .limit
            .as_ref()
            .map(|limit| Limit {
                context: limit.context,
                input: limit.input,
                output: limit.output,
            })
            .unwrap_or_default(),
        modalities: model
            .modalities
            .as_ref()
            .map(|modalities| Modalities {
                input: modalities.input.clone(),
                output: modalities.output.clone(),
            })
            .unwrap_or_default(),
    }
}

fn serving(model: wire::WireModel) -> Serving {
    Serving {
        cost: model.cost.map(|cost| Cost {
            input: cost.input,
            output: cost.output,
            cache_read: cost.cache_read,
            cache_write: cost.cache_write,
            input_audio: cost.input_audio,
            output_audio: cost.output_audio,
            reasoning: cost.reasoning,
            other: cost.other,
        }),
        reasoning_options: model.reasoning_options,
        interleaved: model.interleaved,
        status: model.status.and_then(non_empty),
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature catalog exercising each way an offering relates to a
    /// creator model: served under the creator's full id, served by the
    /// creator under the bare id, linked only by inherited metadata, and not
    /// linked at all.
    const SAMPLE: &str = r#"
    {
      "providers": {
        "anthropic": {
          "id": "anthropic",
          "name": "Anthropic",
          "npm": "@ai-sdk/anthropic",
          "env": ["ANTHROPIC_API_KEY"],
          "doc": "https://docs.anthropic.com",
          "models": {
            "claude-opus-5": {
              "id": "claude-opus-5",
              "name": "Claude Opus 5",
              "description": "Frontier model",
              "family": "claude-opus",
              "attachment": true,
              "reasoning": true,
              "tool_call": true,
              "structured_output": true,
              "temperature": true,
              "knowledge": "2026-01",
              "release_date": "2026-03-01",
              "last_updated": "2026-03-01",
              "modalities": { "input": ["text", "image"], "output": ["text"] },
              "open_weights": false,
              "limit": { "context": 1000000, "output": 128000 },
              "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }],
              "cost": { "input": 5, "output": 25, "cache_read": 0.5,
                        "context_over_200k": { "input": 10, "output": 37.5 } }
            }
          }
        },
        "router": {
          "id": "router",
          "name": "Router",
          "npm": "@ai-sdk/openai-compatible",
          "env": ["ROUTER_KEY"],
          "doc": "https://router.example/models",
          "api": "https://router.example/v1",
          "models": {
            "anthropic/claude-opus-5": {
              "id": "anthropic/claude-opus-5",
              "name": "Claude Opus 5",
              "description": "Frontier model",
              "family": "claude-opus",
              "attachment": true,
              "reasoning": true,
              "tool_call": true,
              "structured_output": true,
              "temperature": true,
              "knowledge": "2026-01",
              "release_date": "2026-03-01",
              "last_updated": "2026-03-01",
              "modalities": { "input": ["text", "image"], "output": ["text"] },
              "open_weights": false,
              "limit": { "context": 200000, "output": 64000 },
              "cost": { "input": 6, "output": 30 }
            },
            "Qwen/Tiny-Instruct": {
              "id": "Qwen/Tiny-Instruct",
              "name": "Tiny Instruct",
              "description": "Small open model",
              "family": "qwen",
              "attachment": false,
              "reasoning": false,
              "tool_call": true,
              "release_date": "2025-06-01",
              "last_updated": "2025-06-01",
              "modalities": { "input": ["text"], "output": ["text"] },
              "open_weights": true,
              "limit": { "context": 32768 },
              "cost": { "input": 0.1, "output": 0.2 }
            },
            "other/generic": {
              "id": "other/generic",
              "description": "Small open model",
              "family": "qwen",
              "attachment": false,
              "reasoning": false,
              "tool_call": false,
              "release_date": "2024-01-01",
              "last_updated": "2024-01-01",
              "open_weights": true
            },
            "house-special": {
              "id": "house-special",
              "name": "House Special",
              "attachment": false,
              "reasoning": true,
              "tool_call": false,
              "release_date": "2026-01",
              "last_updated": "2026-01",
              "open_weights": false,
              "status": "beta"
            }
          }
        }
      },
      "models": {
        "anthropic/claude-opus-5": {
          "id": "anthropic/claude-opus-5",
          "name": "Claude Opus 5",
          "description": "Frontier model",
          "family": "claude-opus",
          "attachment": true,
          "reasoning": true,
          "tool_call": true,
          "structured_output": true,
          "temperature": true,
          "knowledge": "2026-01",
          "release_date": "2026-03-01",
          "last_updated": "2026-03-01",
          "modalities": { "input": ["text", "image"], "output": ["text"] },
          "open_weights": false,
          "limit": { "context": 1000000, "output": 128000 }
        },
        "alibaba/tiny-instruct": {
          "id": "alibaba/tiny-instruct",
          "name": "Tiny Instruct",
          "description": "Small open model",
          "family": "qwen",
          "attachment": false,
          "reasoning": false,
          "tool_call": true,
          "release_date": "2025-06-01",
          "last_updated": "2025-06-01",
          "modalities": { "input": ["text"], "output": ["text"] },
          "open_weights": true,
          "license": "Apache-2.0",
          "limit": { "context": 32768 }
        }
      }
    }"#;

    fn catalog() -> Catalog {
        SAMPLE.parse().expect("sample parses")
    }

    #[test]
    fn counts_each_unit() {
        let catalog = catalog();
        assert_eq!(catalog.providers().len(), 2);
        assert_eq!(catalog.models().len(), 2);
        assert_eq!(catalog.len(), 5);
        assert_eq!(catalog.provider("router").unwrap().model_count, 4);
    }

    #[test]
    fn a_creator_model_knows_its_creator_and_license() {
        let catalog = catalog();
        let tiny = catalog.model("alibaba/tiny-instruct").unwrap();
        assert_eq!(tiny.creator, "alibaba");
        assert_eq!(tiny.license.as_deref(), Some("Apache-2.0"));
    }

    #[test]
    fn provider_metadata_comes_through() {
        let catalog = catalog();
        let router = catalog.provider("router").unwrap();
        assert_eq!(router.npm.as_deref(), Some("@ai-sdk/openai-compatible"));
        assert_eq!(router.env, vec!["ROUTER_KEY"]);
        assert_eq!(router.api.as_deref(), Some("https://router.example/v1"));
        assert_eq!(catalog.provider("anthropic").unwrap().api, None);
    }

    #[test]
    fn a_creator_serving_its_own_model_links_by_bare_id_and_stores_nothing_twice() {
        let catalog = catalog();
        let opus = catalog.offering("anthropic", "claude-opus-5").unwrap();
        assert_eq!(opus.base_model.as_deref(), Some("anthropic/claude-opus-5"));
        assert_eq!(opus.overrides, Overrides::default());
    }

    #[test]
    fn a_router_links_by_full_id_and_keeps_only_what_differs() {
        let catalog = catalog();
        let opus = catalog
            .offering("router", "anthropic/claude-opus-5")
            .unwrap();
        assert_eq!(opus.base_model.as_deref(), Some("anthropic/claude-opus-5"));
        assert_eq!(
            opus.overrides,
            Overrides {
                limit: Some(Limit {
                    context: Some(200_000),
                    input: None,
                    output: Some(64_000),
                }),
                ..Default::default()
            }
        );
    }

    #[test]
    fn inherited_metadata_links_a_renamed_offering() {
        let catalog = catalog();
        let tiny = catalog.offering("router", "Qwen/Tiny-Instruct").unwrap();
        assert_eq!(tiny.base_model.as_deref(), Some("alibaba/tiny-instruct"));
        assert_eq!(tiny.overrides, Overrides::default());
    }

    #[test]
    fn a_shared_description_alone_is_not_a_link() {
        // Same description and family as `alibaba/tiny-instruct`, different
        // release date: a collision, not an inheritance.
        let catalog = catalog();
        let generic = catalog.offering("router", "other/generic").unwrap();
        assert_eq!(generic.base_model, None);
    }

    #[test]
    fn an_unlinked_offering_carries_every_field() {
        let catalog = catalog();
        let special = catalog.offering("router", "house-special").unwrap();
        assert_eq!(special.base_model, None);
        assert_eq!(special.overrides.name.as_deref(), Some("House Special"));
        assert_eq!(special.overrides.reasoning, Some(true));
        assert_eq!(special.serving.status.as_deref(), Some("beta"));
    }

    #[test]
    fn every_offering_resolves_to_what_the_file_said() {
        let catalog = catalog();
        let wire: wire::WireCatalog = serde_json::from_str(SAMPLE).unwrap();
        for (provider_key, provider) in &wire.providers {
            for (id, model) in &provider.models {
                let offering = catalog.offering(provider_key, id).unwrap();
                assert_eq!(catalog.resolve(offering), spec(id, model), "{provider_key}/{id}");
            }
        }
    }

    #[test]
    fn a_dropped_field_leaves_the_offering_unlinked_rather_than_wrong() {
        let base = Spec {
            name: "Base".into(),
            knowledge: Some("2025-01".into()),
            ..Default::default()
        };
        let offering = Spec {
            name: "Base".into(),
            ..Default::default()
        };
        assert_eq!(Overrides::diff(&offering, &base), None);
    }

    #[test]
    fn cost_keeps_what_it_does_not_model() {
        let catalog = catalog();
        let cost = catalog
            .offering("anthropic", "claude-opus-5")
            .unwrap()
            .serving
            .cost
            .clone()
            .unwrap();
        assert_eq!(cost.input, Some(5.0));
        assert_eq!(cost.cache_write, None);
        assert!(cost.other.contains_key("context_over_200k"));
    }

    #[test]
    fn vision_comes_from_the_image_modality() {
        let catalog = catalog();
        assert!(catalog.model("anthropic/claude-opus-5").unwrap().spec.vision());
        assert!(!catalog.model("alibaba/tiny-instruct").unwrap().spec.vision());
    }

    #[test]
    fn a_missing_name_falls_back_to_the_id() {
        let catalog = catalog();
        let generic = catalog.offering("router", "other/generic").unwrap();
        assert_eq!(catalog.resolve(generic).name, "other/generic");
    }

    #[test]
    fn the_empty_preset_serializes_in_the_catalog_shape() {
        let json = serde_json::to_value(Preset::default()).unwrap();
        for key in ["provider", "id", "base_model", "name", "tool_call", "limit", "modalities", "cost"] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
    }

    /// The real catalog, fetched: every offering resolves to exactly the entry
    /// the file published. Run with `cargo test -p presets -- --ignored`.
    #[tokio::test]
    #[ignore = "fetches the live catalog"]
    async fn the_live_catalog_round_trips() {
        let bytes = reqwest::get(crate::DEFAULT_URL)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let catalog = Catalog::parse(&bytes).unwrap();
        let wire: wire::WireCatalog = serde_json::from_slice(&bytes).unwrap();
        let mut linked = 0;
        for (provider_key, provider) in &wire.providers {
            let provider_id = if provider.id.trim().is_empty() {
                provider_key
            } else {
                &provider.id
            };
            for (key, model) in &provider.models {
                let id = if model.id.trim().is_empty() { key } else { &model.id };
                let offering = catalog.offering(provider_id, id).unwrap();
                linked += usize::from(offering.base_model.is_some());
                assert_eq!(catalog.resolve(offering), spec(id, model), "{provider_id}/{id}");
            }
        }
        eprintln!("{linked} of {} offerings linked", catalog.len());
    }
}
