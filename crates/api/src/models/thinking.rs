//! The thinking knob: what a model offers, what a session picked, and what
//! the two together mean for a run.
//!
//! Split across the two rows on purpose. *Whether a model reasons at all, and
//! at which levels*, is a fact about the endpoint behind it and belongs to the
//! model definition — the same place `reasoning_history` and the advanced
//! options live. *How hard this conversation should think* is the user's, and
//! belongs to the session. Neither half is meaningful alone: a selection is
//! read against the model it will run on, and this module is where that
//! reading happens.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a model definition says about reasoning.
///
/// Lives under `capabilities.thinking`. A row that says nothing has no
/// thinking knob at all: the picker hides it and a run against it sends no
/// reasoning fields, which is exactly what every row written before this
/// setting existed needs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThinkingCapability {
    /// Whether this model reasons at all. Everything else here is inert
    /// without it.
    #[serde(default)]
    pub supported: bool,
    /// The effort levels worth offering for this model, in the order the
    /// picker shows them. Empty is a real answer, not an omission: a model
    /// that reasons but takes no effort field has an on/off knob and nothing
    /// more.
    #[serde(default)]
    pub efforts: Vec<llm::Effort>,
    /// What a session that never picked gets. `None` leaves the provider's
    /// own default, which is also what a model with no opinion here wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<llm::Effort>,
}

impl ThinkingCapability {
    pub fn offers(&self, effort: llm::Effort) -> bool {
        self.efforts.contains(&effort)
    }

    /// The reasoning fields a run should carry, given what the session picked.
    ///
    /// `None` means this model has no opinion on reasoning and the run should
    /// not acquire one — see [`ResolvedThinking`].
    ///
    /// A selection this model does not offer is clamped to the model's own
    /// default rather than refused: the selection outlives the model it was
    /// made against (switching models mid-session is one click, and a model's
    /// levels can be edited afterwards), and a session that will not run
    /// because of a knob is worse than one that thinks a little differently
    /// than it was last told to.
    pub fn resolve(&self, selection: Option<ThinkingSelection>) -> Option<ResolvedThinking> {
        if !self.supported {
            return None;
        }

        let resolved = match selection {
            Some(ThinkingSelection::Off) => ResolvedThinking {
                thinking: Some(llm::Thinking::Disabled),
                effort: None,
            },
            Some(ThinkingSelection::On) => ResolvedThinking {
                thinking: Some(Self::adaptive()),
                effort: None,
            },
            Some(ThinkingSelection::Effort(effort)) if self.offers(effort) => ResolvedThinking {
                thinking: Some(Self::adaptive()),
                effort: Some(effort),
            },
            // Either nothing was ever picked, or what was picked is not on
            // offer here. Both land on the model's own default.
            _ => match self.default_effort {
                Some(effort) => ResolvedThinking {
                    thinking: Some(Self::adaptive()),
                    effort: Some(effort),
                },
                None => ResolvedThinking::default(),
            },
        };

        Some(resolved)
    }

    /// Reasoning that was asked for is reasoning somebody wants to read — the
    /// thread surface renders thinking blocks — so the summarized display is
    /// what "on" means here. A harness that wants the reasoning hidden still
    /// overrides it per call.
    fn adaptive() -> llm::Thinking {
        llm::Thinking::Adaptive {
            display: llm::ThinkingDisplay::Summarized,
        }
    }
}

/// What a session picked, stored as text on `session.thinking_effort` and
/// carried over the API as the same string.
///
/// Not `llm::Effort` with a nullable column standing in for "off": "off" and
/// "never picked" are different answers — one sends `thinking: disabled`, the
/// other sends nothing and lets the model's definition decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingSelection {
    /// Don't reason on this session's turns.
    Off,
    /// Reason, at whatever effort the provider picks for itself. What a model
    /// that reasons but offers no levels can be set to.
    On,
    /// Reason at this level.
    Effort(llm::Effort),
}

impl ThinkingSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Effort(llm::Effort::Low) => "low",
            Self::Effort(llm::Effort::Medium) => "medium",
            Self::Effort(llm::Effort::High) => "high",
            Self::Effort(llm::Effort::XHigh) => "xhigh",
            Self::Effort(llm::Effort::Max) => "max",
        }
    }

    /// Reads a stored or submitted value, naming what is wrong with it rather
    /// than falling back — every caller of this either writes the value or
    /// reads a value some earlier write already accepted.
    pub fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "off" => Self::Off,
            "on" => Self::On,
            "low" => Self::Effort(llm::Effort::Low),
            "medium" => Self::Effort(llm::Effort::Medium),
            "high" => Self::Effort(llm::Effort::High),
            "xhigh" => Self::Effort(llm::Effort::XHigh),
            "max" => Self::Effort(llm::Effort::Max),
            other => {
                return Err(format!(
                    "'{other}' is not a thinking selection — use \"off\", \"on\", or one of \
                     \"low\", \"medium\", \"high\", \"xhigh\", \"max\""
                ));
            }
        })
    }

    /// A stored value that no longer parses, dropped rather than raised.
    ///
    /// The write path validates, so a row this can't read went around the API
    /// — and the model's own default is a better answer for it than a session
    /// that refuses to run.
    pub fn from_stored(value: Option<&str>) -> Option<Self> {
        value.and_then(|value| Self::parse(value).ok())
    }
}

/// The reasoning half of a request, as one run resolved it.
///
/// Both fields `None` is a real value and not an empty one: it says this run
/// has an opinion — the user picked "provider default" — and that opinion is
/// to send neither field. That is distinct from [`ThinkingCapability::resolve`]
/// returning `None`, which says the run has no opinion at all and should leave
/// whatever the conversation was already running with untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResolvedThinking {
    pub thinking: Option<llm::Thinking>,
    pub effort: Option<llm::Effort>,
}

/// Reads a `capabilities.thinking` value, naming what is wrong with it rather
/// than falling back — the write path wants the complaint.
///
/// Checked at write time for the same reason `reasoning_history` is: a typo
/// that quietly means "no thinking knob" is a setting the user believes they
/// made and would never see fail.
pub fn parse_thinking_capability(value: &Value) -> Result<ThinkingCapability, String> {
    if value.is_null() {
        return Ok(ThinkingCapability::default());
    }

    let capability: ThinkingCapability = serde_json::from_value(value.clone()).map_err(|_| {
        "thinking must be an object with an optional \"supported\" boolean, an optional \
         \"efforts\" array of \"low\"/\"medium\"/\"high\"/\"xhigh\"/\"max\", and an optional \
         \"default_effort\" naming one of them"
            .to_owned()
    })?;

    if !capability.supported
        && (!capability.efforts.is_empty() || capability.default_effort.is_some())
    {
        return Err(
            "thinking.efforts and thinking.default_effort need thinking.supported: true".to_owned(),
        );
    }

    for (index, effort) in capability.efforts.iter().enumerate() {
        if capability.efforts[..index].contains(effort) {
            return Err("thinking.efforts must not list the same level twice".to_owned());
        }
    }

    if let Some(default) = capability.default_effort
        && !capability.offers(default)
    {
        return Err("thinking.default_effort must be one of thinking.efforts".to_owned());
    }

    Ok(capability)
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::Effort;
    use serde_json::json;

    fn capability(
        supported: bool,
        efforts: &[Effort],
        default: Option<Effort>,
    ) -> ThinkingCapability {
        ThinkingCapability {
            supported,
            efforts: efforts.to_vec(),
            default_effort: default,
        }
    }

    fn adaptive() -> llm::Thinking {
        llm::Thinking::Adaptive {
            display: llm::ThinkingDisplay::Summarized,
        }
    }

    #[test]
    fn a_model_that_does_not_reason_leaves_the_run_alone() {
        let capability = ThinkingCapability::default();
        assert_eq!(capability.resolve(None), None);
        // Even a selection cannot give a model a knob its definition says it
        // does not have.
        assert_eq!(
            capability.resolve(Some(ThinkingSelection::Effort(Effort::High))),
            None
        );
    }

    #[test]
    fn each_selection_reaches_the_run_as_itself() {
        let capability = capability(true, &[Effort::Low, Effort::High], None);

        assert_eq!(
            capability.resolve(Some(ThinkingSelection::Off)),
            Some(ResolvedThinking {
                thinking: Some(llm::Thinking::Disabled),
                effort: None,
            })
        );
        assert_eq!(
            capability.resolve(Some(ThinkingSelection::On)),
            Some(ResolvedThinking {
                thinking: Some(adaptive()),
                effort: None,
            })
        );
        assert_eq!(
            capability.resolve(Some(ThinkingSelection::Effort(Effort::High))),
            Some(ResolvedThinking {
                thinking: Some(adaptive()),
                effort: Some(Effort::High),
            })
        );
    }

    #[test]
    fn nothing_picked_falls_to_the_models_own_default() {
        let with_default = capability(true, &[Effort::Low, Effort::Medium], Some(Effort::Medium));
        assert_eq!(
            with_default.resolve(None),
            Some(ResolvedThinking {
                thinking: Some(adaptive()),
                effort: Some(Effort::Medium),
            })
        );

        // Supported, but with nothing to default to: the run says it has an
        // opinion and that opinion is to send neither field.
        let without_default = capability(true, &[], None);
        assert_eq!(
            without_default.resolve(None),
            Some(ResolvedThinking::default())
        );
    }

    #[test]
    fn a_level_this_model_does_not_offer_is_clamped_rather_than_refused() {
        let capability = capability(true, &[Effort::Low], Some(Effort::Low));
        assert_eq!(
            capability.resolve(Some(ThinkingSelection::Effort(Effort::Max))),
            capability.resolve(None),
            "a selection made against another model must not fail this one"
        );
    }

    #[test]
    fn a_selection_round_trips_through_its_stored_form() {
        for selection in [
            ThinkingSelection::Off,
            ThinkingSelection::On,
            ThinkingSelection::Effort(Effort::Low),
            ThinkingSelection::Effort(Effort::Medium),
            ThinkingSelection::Effort(Effort::High),
            ThinkingSelection::Effort(Effort::XHigh),
            ThinkingSelection::Effort(Effort::Max),
        ] {
            assert_eq!(ThinkingSelection::parse(selection.as_str()), Ok(selection));
        }
    }

    #[test]
    fn a_row_written_around_the_api_falls_back_rather_than_failing_the_run() {
        assert_eq!(ThinkingSelection::from_stored(Some("sometimes")), None);
        assert_eq!(ThinkingSelection::from_stored(None), None);
        assert_eq!(
            ThinkingSelection::from_stored(Some("high")),
            Some(ThinkingSelection::Effort(Effort::High))
        );
    }

    #[test]
    fn the_write_path_gets_told_what_is_wrong_with_the_capability() {
        assert_eq!(
            parse_thinking_capability(&json!(null)),
            Ok(ThinkingCapability::default())
        );
        assert_eq!(
            parse_thinking_capability(&json!({ "supported": true, "efforts": ["low", "high"] })),
            Ok(capability(true, &[Effort::Low, Effort::High], None))
        );

        assert!(parse_thinking_capability(&json!("yes please")).is_err());
        assert!(parse_thinking_capability(&json!({ "efforts": ["sometimes"] })).is_err());
        // Levels without support are a contradiction, not a default.
        assert!(parse_thinking_capability(&json!({ "efforts": ["low"] })).is_err());
        assert!(
            parse_thinking_capability(&json!({ "supported": true, "efforts": ["low", "low"] }))
                .is_err()
        );
        assert!(
            parse_thinking_capability(&json!({
                "supported": true,
                "efforts": ["low"],
                "default_effort": "high"
            }))
            .is_err()
        );
    }
}
