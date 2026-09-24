//! The read-only catalog: what the directory publishes, shaped for callers.
//!
//! A [`Preset`] says what a model can do and what it costs. It is not a
//! configured model and can never become one by accident — nothing here is
//! persisted, and a preset carries no `base_url` a request could be sent to.
//! The directory lists *offers*; a deployment's own models are rows a user
//! wrote.
//!
//! Serialization is part of the public contract: the catalog is already a
//! projection of somebody else's JSON, and a caller that exposes it (the
//! `api` crate's browse routes) needs the same shape this crate already
//! settled on. Remapping it into a second, parallel set of response structs
//! would only add a place for the two to disagree.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, wire};

/// Every provider and model the directory publishes, indexed for lookup.
#[derive(Debug)]
pub struct Catalog {
    providers: Vec<Provider>,
    presets: Vec<Preset>,
    /// `(provider_id, model_id)` → index into `presets`.
    index: HashMap<(String, String), usize>,
}

impl Catalog {
    /// Reads the directory from its bytes. Pure — no network, no state.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let directory: wire::Directory = serde_json::from_slice(bytes)?;
        Ok(Self::build(directory))
    }

    fn build(directory: wire::Directory) -> Self {
        let mut providers = Vec::with_capacity(directory.len());
        let mut presets = Vec::new();
        let mut index = HashMap::new();

        for (key, provider) in directory {
            // The key and the provider's own `id` agree today; preferring the
            // id keeps the lookup key stable if the map key is ever renamed.
            let provider_id = non_empty(provider.id).unwrap_or_else(|| key.clone());
            let provider_name = provider
                .name
                .and_then(non_empty)
                .unwrap_or_else(|| provider_id.clone());

            let mut model_count = 0usize;
            for (map_key, model) in provider.models {
                let id = non_empty(model.id).unwrap_or_else(|| map_key.clone());
                let name = model.name.and_then(non_empty).unwrap_or_else(|| id.clone());

                let capabilities = Capabilities {
                    // "Vision" is image *input*, which the directory states as
                    // a modality rather than a feature flag.
                    vision: model
                        .modalities
                        .input
                        .iter()
                        .any(|modality| modality.eq_ignore_ascii_case("image")),
                    attachment: model.features.attachment,
                    reasoning: model.features.reasoning,
                    tools: model.features.tool_call,
                    structured_output: model.features.structured_output,
                    temperature: model.features.temperature,
                };

                let preset = Preset {
                    provider: provider_id.clone(),
                    provider_name: provider_name.clone(),
                    id: id.clone(),
                    name,
                    capabilities,
                    pricing: Pricing {
                        input: model.pricing.input,
                        output: model.pricing.output,
                        cache_read: model.pricing.cache_read,
                        cache_write: model.pricing.cache_write,
                        input_audio: model.pricing.input_audio,
                        output_audio: model.pricing.output_audio,
                        reasoning: model.pricing.reasoning,
                    },
                    limits: Limits {
                        context: model.limit.context,
                        input: model.limit.input,
                        output: model.limit.output,
                    },
                    modalities: Modalities {
                        input: model.modalities.input,
                        output: model.modalities.output,
                    },
                    release_date: epoch(&model.release_date),
                    last_updated: epoch(&model.last_updated),
                    knowledge_cutoff: epoch(&model.knowledge_cutoff),
                    open_weights: model.open_weights,
                };

                index.insert((provider_id.clone(), id), presets.len());
                presets.push(preset);
                model_count += 1;
            }

            providers.push(Provider {
                id: provider_id,
                name: provider_name,
                website: provider.website,
                api_base_url: provider.api_base_url,
                model_count,
            });
        }

        Self {
            providers,
            presets,
            index,
        }
    }

    /// Providers in the directory's alphabetical order.
    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    /// Every model, providers and models both alphabetically ordered.
    pub fn presets(&self) -> &[Preset] {
        &self.presets
    }

    pub fn len(&self) -> usize {
        self.presets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// One model, by the provider that publishes it and the id it publishes
    /// it under.
    pub fn get(&self, provider: &str, id: &str) -> Option<&Preset> {
        self.index
            .get(&(provider.to_owned(), id.to_owned()))
            .map(|&index| &self.presets[index])
    }

    /// The models matching `query`, in catalog order.
    ///
    /// Filtering here is in memory: this crate holds the catalog whole. A
    /// caller that stores it — the service does, in its own table — can filter
    /// in that store instead; this remains the answer for the file as loaded,
    /// where a linear scan of twelve thousand rows is cheaper than an index.
    pub fn filter(&self, query: &Query) -> Vec<&Preset> {
        let needle = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|needle| !needle.is_empty())
            .map(str::to_lowercase);

        self.presets
            .iter()
            .filter(|preset| {
                if let Some(provider) = query.provider.as_deref()
                    && preset.provider != provider
                {
                    return false;
                }
                if let Some(needle) = &needle
                    && !["id", "name", "provider", "provider_name"]
                        .iter()
                        .any(|field| match *field {
                            "id" => preset.id.to_lowercase().contains(needle),
                            "name" => preset.name.to_lowercase().contains(needle),
                            "provider" => preset.provider.to_lowercase().contains(needle),
                            _ => preset.provider_name.to_lowercase().contains(needle),
                        })
                {
                    return false;
                }
                if let Some(wanted) = query.vision
                    && preset.capabilities.vision != wanted
                {
                    return false;
                }
                if let Some(wanted) = query.reasoning
                    && preset.capabilities.reasoning != wanted
                {
                    return false;
                }
                if let Some(wanted) = query.tools
                    && preset.capabilities.tools != wanted
                {
                    return false;
                }
                true
            })
            .collect()
    }
}

impl std::str::FromStr for Catalog {
    type Err = Error;

    /// The same as [`Catalog::parse`], from a JSON string.
    fn from_str(json: &str) -> Result<Self, Self::Err> {
        Self::parse(json.as_bytes())
    }
}

/// What to narrow a browse to. Every field is optional; an absent one does
/// not filter. A capability set to `false` requires its *absence*, which is a
/// different question from not asking.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// A provider key, e.g. `anthropic`.
    pub provider: Option<String>,
    /// Case-insensitive substring of a model or provider id or name.
    pub search: Option<String>,
    pub vision: Option<bool>,
    pub reasoning: Option<bool>,
    pub tools: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub website: Option<String>,
    /// The provider's default API base, when it names one. Informational: a
    /// preset never routes a request.
    pub api_base_url: Option<String>,
    pub model_count: usize,
}

/// One model as the directory publishes it.
///
/// [`Default`] is the built-in empty preset: every capability unstated, every
/// price unknown, no limits, no modalities. It is what a configured model with
/// no `preset_id` is read against — a description that says nothing and so
/// works for any model, rather than an error or a missing metadata blob.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// The publisher's key, e.g. `anthropic`.
    pub provider: String,
    /// The publisher's display name, e.g. `Anthropic`.
    pub provider_name: String,
    /// The model id as served, e.g. `claude-opus-5`.
    pub id: String,
    pub name: String,
    pub capabilities: Capabilities,
    pub pricing: Pricing,
    pub limits: Limits,
    pub modalities: Modalities,
    /// Epoch seconds, when stated.
    pub release_date: Option<i64>,
    pub last_updated: Option<i64>,
    pub knowledge_cutoff: Option<i64>,
    pub open_weights: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    /// Image input — derived from the input modalities, not a feature flag.
    pub vision: bool,
    pub attachment: bool,
    pub reasoning: bool,
    pub tools: bool,
    pub structured_output: bool,
    /// Whether the endpoint takes a temperature at all. Absent in the file for
    /// most models, which reads as `false` — meaning "unstated", not "refuses".
    pub temperature: bool,
}

/// US dollars per million tokens. `None` is *unknown*, not free: a provider
/// that does not bill a component simply omits it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pricing {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub input_audio: Option<f64>,
    pub output_audio: Option<f64>,
    pub reasoning: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Total context window, in tokens.
    pub context: Option<u64>,
    /// Maximum prompt, where a provider states one separately.
    pub input: Option<u64>,
    /// Maximum output, where a provider states one.
    pub output: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Modalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

/// Reads an epoch-seconds stamp, which the file writes as a string and very
/// occasionally as the number `0`. A zero is "unstated", not 1970.
fn epoch(value: &Option<Value>) -> Option<i64> {
    let value = value.as_ref()?;
    let seconds = match value {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.parse::<i64>().ok()?,
        _ => return None,
    };
    (seconds > 0).then_some(seconds)
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature directory exercising the shapes the real file uses:
    /// ints and floats in pricing, string epochs, a model missing its name,
    /// and a provider whose key is also its id.
    const SAMPLE: &str = r#"
    {
      "anthropic": {
        "id": "anthropic",
        "name": "Anthropic",
        "website": "https://anthropic.com",
        "apiBaseUrl": "https://api.anthropic.com",
        "models": {
          "claude-opus-5": {
            "id": "claude-opus-5",
            "name": "Claude Opus 5",
            "release_date": "1729036800",
            "features": {
              "attachment": true,
              "reasoning": true,
              "tool_call": true,
              "structured_output": true,
              "temperature": false
            },
            "pricing": { "input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75 },
            "limit": { "context": 200000, "output": 64000 },
            "modalities": { "input": ["text", "image"], "output": ["text"] },
            "open_weights": false
          },
          "claude-haiku": {
            "id": "claude-haiku",
            "name": "Claude Haiku",
            "features": { "reasoning": false, "tool_call": true },
            "pricing": { "input": 0.8, "output": 4 },
            "limit": { "context": 200000 },
            "modalities": { "input": ["text"], "output": ["text"] }
          }
        }
      },
      "local": {
        "id": "local",
        "models": {
          "tiny": {
            "release_date": "0",
            "features": { "reasoning": true }
          }
        }
      }
    }"#;

    fn catalog() -> Catalog {
        SAMPLE.parse().expect("sample parses")
    }

    #[test]
    fn counts_providers_and_models() {
        let catalog = catalog();
        assert_eq!(catalog.providers().len(), 2);
        assert_eq!(catalog.len(), 3);
        assert!(!catalog.is_empty());
    }

    #[test]
    fn vision_comes_from_the_image_modality() {
        let catalog = catalog();
        let opus = catalog.get("anthropic", "claude-opus-5").unwrap();
        assert!(opus.capabilities.vision);
        let haiku = catalog.get("anthropic", "claude-haiku").unwrap();
        assert!(!haiku.capabilities.vision);
    }

    #[test]
    fn reasoning_and_tools_come_from_features() {
        let catalog = catalog();
        let opus = catalog.get("anthropic", "claude-opus-5").unwrap();
        assert!(opus.capabilities.reasoning);
        assert!(opus.capabilities.tools);
        let haiku = catalog.get("anthropic", "claude-haiku").unwrap();
        assert!(!haiku.capabilities.reasoning);
        assert!(haiku.capabilities.tools);
    }

    #[test]
    fn prices_and_context_reach_the_caller_as_numbers() {
        let catalog = catalog();
        let opus = catalog.get("anthropic", "claude-opus-5").unwrap();
        assert_eq!(opus.pricing.input, Some(3.0));
        assert_eq!(opus.pricing.output, Some(15.0));
        assert_eq!(opus.pricing.cache_read, Some(0.3));
        assert_eq!(opus.limits.context, Some(200_000));
        assert_eq!(opus.limits.output, Some(64_000));
        // A component nobody priced is unknown, not free.
        assert_eq!(opus.pricing.input_audio, None);
    }

    #[test]
    fn a_missing_name_falls_back_to_the_id() {
        let catalog = catalog();
        let tiny = catalog.get("local", "tiny").unwrap();
        assert_eq!(tiny.name, "tiny");
        // And a provider with no name falls back to its id too.
        let provider = catalog.provider("local").unwrap();
        assert_eq!(provider.name, "local");
    }

    #[test]
    fn a_zero_epoch_is_unstated_not_1970() {
        let catalog = catalog();
        let tiny = catalog.get("local", "tiny").unwrap();
        assert_eq!(tiny.release_date, None);
    }

    #[test]
    fn an_epoch_string_becomes_seconds() {
        let catalog = catalog();
        let opus = catalog.get("anthropic", "claude-opus-5").unwrap();
        assert_eq!(opus.release_date, Some(1_729_036_800));
    }

    #[test]
    fn provider_model_count_only_counts_its_own_models() {
        let catalog = catalog();
        assert_eq!(catalog.provider("anthropic").unwrap().model_count, 2);
        assert_eq!(catalog.provider("local").unwrap().model_count, 1);
    }

    #[test]
    fn filter_narrows_by_provider_and_capability() {
        let catalog = catalog();
        let reasoning = catalog.filter(&Query {
            reasoning: Some(true),
            ..Default::default()
        });
        assert_eq!(
            reasoning.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["claude-opus-5", "tiny"]
        );

        let anthropic_vision = catalog.filter(&Query {
            provider: Some("anthropic".into()),
            vision: Some(true),
            ..Default::default()
        });
        assert_eq!(
            anthropic_vision
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-opus-5"]
        );
    }

    #[test]
    fn filter_search_matches_ids_names_and_providers_case_insensitively() {
        let catalog = catalog();
        let by_name = catalog.filter(&Query {
            search: Some("OPUS".into()),
            ..Default::default()
        });
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].id, "claude-opus-5");

        let by_provider = catalog.filter(&Query {
            search: Some("anthropic".into()),
            ..Default::default()
        });
        assert_eq!(by_provider.len(), 2);
    }

    #[test]
    fn a_false_capability_asks_for_its_absence() {
        let catalog = catalog();
        let no_reasoning = catalog.filter(&Query {
            reasoning: Some(false),
            ..Default::default()
        });
        assert_eq!(
            no_reasoning
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-haiku"]
        );
    }

    #[test]
    fn an_unknown_provider_has_no_models_and_no_provider() {
        let catalog = catalog();
        assert!(catalog.get("nope", "nope").is_none());
        assert!(catalog.provider("nope").is_none());
        assert!(
            catalog
                .filter(&Query {
                    provider: Some("nope".into()),
                    ..Default::default()
                })
                .is_empty()
        );
    }
}
