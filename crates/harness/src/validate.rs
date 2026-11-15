//! The §7 validation surface: everything `Context::stream` can refuse a
//! request for, before it ever touches the network.
//!
//! Pure and provider-neutral. Holds no `HarnessState` borrow — it takes
//! exactly the slices of state each check needs and returns either the
//! resolved turns to send or a typed [`Refusal`]. Checks run in a fixed
//! order (prefix, then content, then structure, then scaffold) so a
//! wholly-misordered request reports the prefix failure rather than a
//! confusing downstream one.

use std::collections::HashMap;

use crate::canonical::canonical_json;
use crate::mapping::{Message as WireMessage, WireTurn};
use crate::scaffold;
use crate::state::{Committed, ScaffoldPrint, SentTurn};

/// Everything `check` can refuse a request for (`proposal.md` §7), minus the
/// `tool_removal` row — §5 (mid-conversation tool changes) is deferred, so
/// there is nothing yet that could orphan a removal.
#[derive(Debug, Clone)]
pub enum Refusal {
    RefNotPrefix { message: String },
    RefContentMismatch { id: String },
    MalformedTurns { message: String },
    ScaffoldMismatch { component: &'static str },
    IncompleteCompletion,
}

impl Refusal {
    pub fn kind(&self) -> &'static str {
        match self {
            Refusal::RefNotPrefix { .. } => "ref_not_prefix",
            Refusal::RefContentMismatch { .. } => "ref_content_mismatch",
            Refusal::MalformedTurns { .. } => "malformed_turns",
            Refusal::ScaffoldMismatch { .. } => "scaffold_mismatch",
            Refusal::IncompleteCompletion => "incomplete_completion",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Refusal::RefNotPrefix { message } => message.clone(),
            Refusal::RefContentMismatch { id } => format!(
                "supplied content for `{id}` does not match what Core has stored for it — \
                 strip the id to send an edited version instead of an edited copy of it"
            ),
            Refusal::MalformedTurns { message } => message.clone(),
            Refusal::ScaffoldMismatch { component } => format!(
                "the request's {component} does not match the referenced lineage's — a \
                 harness that means to diverge should not reference ids from it"
            ),
            Refusal::IncompleteCompletion => "the call ended with a truncated completion; \
                 commit(call, { partial: true }) to accept it anyway"
                .to_string(),
        }
    }
}

/// Runs every surviving §7 check against a supplied turn list, resolving it
/// against `lineage`. `tools`/`provider`/`model` are the request's merged
/// scaffold inputs — the caller has already applied §4's defaulting before
/// calling this.
pub fn check(
    supplied: &[WireTurn],
    lineage: &[Committed],
    tools: &[llm::ToolDef],
    provider: &str,
    model: &str,
    scaffold_run: &ScaffoldPrint,
) -> Result<Vec<SentTurn>, Refusal> {
    let mut index_by_id: HashMap<&str, usize> = HashMap::new();
    for (index, committed) in lineage.iter().enumerate() {
        index_by_id.insert(committed.id.as_str(), index);
    }

    let mut sent_turns = Vec::with_capacity(supplied.len());
    let mut resolved_messages: Vec<llm::Message> = Vec::with_capacity(supplied.len());
    let mut ref_count = 0usize;
    let mut seen_value = false;

    for turn in supplied {
        let id = match turn {
            WireTurn::Ref(reference) => Some(reference.id.as_str()),
            WireTurn::Message(message) => message.id.as_deref(),
        };

        match id {
            Some(id) => {
                if seen_value {
                    return Err(Refusal::RefNotPrefix {
                        message: format!(
                            "a by-value turn preceded reference `{id}`; id-bearing turns must \
                             be a contiguous prefix starting at index 0"
                        ),
                    });
                }
                let Some(&lineage_index) = index_by_id.get(id) else {
                    return Err(Refusal::RefNotPrefix {
                        message: format!("no committed message `{id}` in this lineage"),
                    });
                };
                if lineage_index != ref_count {
                    return Err(Refusal::RefNotPrefix {
                        message: format!(
                            "reference `{id}` names lineage position {lineage_index}, expected \
                             {ref_count} — references must name a contiguous prefix, in order"
                        ),
                    });
                }

                let stored = lineage[lineage_index].message.clone();
                if let WireTurn::Message(supplied_message) = turn
                    && !content_matches(&stored, supplied_message)
                {
                    return Err(Refusal::RefContentMismatch { id: id.to_string() });
                }

                sent_turns.push(SentTurn::Ref { id: id.to_string() });
                resolved_messages.push(stored);
                ref_count += 1;
            }
            None => {
                seen_value = true;
                let WireTurn::Message(message) = turn else {
                    unreachable!("a MessageRef always carries an id")
                };
                let value: llm::Message = message.clone().into();
                sent_turns.push(SentTurn::Value(value.clone()));
                resolved_messages.push(value);
            }
        }
    }

    check_tool_pairing(&resolved_messages)?;
    check_system_placement(&resolved_messages)?;

    if ref_count > 0 {
        let scaffold_now = scaffold::print_for(tools, resolved_messages.iter(), provider, model);
        if scaffold_now.tools != scaffold_run.tools {
            return Err(Refusal::ScaffoldMismatch { component: "tools" });
        }
        if scaffold_now.system != scaffold_run.system {
            return Err(Refusal::ScaffoldMismatch {
                component: "system",
            });
        }
        if scaffold_now.model != scaffold_run.model {
            return Err(Refusal::ScaffoldMismatch { component: "model" });
        }
    }

    Ok(sent_turns)
}

/// The one place stored and supplied content are compared. Both are
/// canonicalized through the same JSON form before comparing, so a faithful
/// JSON round trip — which may lose `u64 > 2^53` precision and fold `1.0`
/// into `1` inside `Unknown.raw` — is never mistaken for a genuine edit, while an
/// actual edit still differs.
fn content_matches(stored: &llm::Message, supplied: &WireMessage) -> bool {
    // `id` is compared separately by the caller (it's how `stored` was found
    // in the first place) — comparing it again here would compare `stored`'s
    // *absent* id (`WireMessage::from(&llm::Message)` always sets `id: None`,
    // since that conversion exists for by-value messages) against whatever
    // id the harness legitimately kept on `supplied`, which would refuse
    // every faithful echo for a reason that has nothing to do with content.
    let stored_wire = WireMessage::from(stored); // id: None
    let supplied_wire = WireMessage {
        id: None,
        ..supplied.clone()
    };
    let stored_json = serde_json::to_value(stored_wire).expect("Message always serializes");
    let supplied_json = serde_json::to_value(supplied_wire).expect("Message always serializes");
    canonical_json(&stored_json) == canonical_json(&supplied_json)
}

/// Every `tool_result` must reference a preceding `tool_use`; every
/// `tool_use` must have a following `tool_result` except in the final
/// message — the model's live call, still awaiting one. An arbitrary slice
/// of the lineage can orphan either side.
fn check_tool_pairing(resolved: &[llm::Message]) -> Result<(), Refusal> {
    let mut open: HashMap<&str, usize> = HashMap::new();

    for (index, message) in resolved.iter().enumerate() {
        for block in &message.content {
            if let llm::ContentBlock::ToolUse { id, .. } = block {
                open.insert(id.as_str(), index);
            }
        }
        for block in &message.content {
            if let llm::ContentBlock::ToolResult { tool_use_id, .. } = block
                && open.remove(tool_use_id.as_str()).is_none()
            {
                return Err(Refusal::MalformedTurns {
                    message: format!("tool_result `{tool_use_id}` has no preceding tool_use"),
                });
            }
        }
    }

    let last_index = resolved.len().saturating_sub(1);
    for (id, at) in &open {
        if *at != last_index {
            return Err(Refusal::MalformedTurns {
                message: format!("tool_use `{id}` has no following tool_result"),
            });
        }
    }
    Ok(())
}

/// Turns `[0..k)` (the leading system run) are the system prompt and are
/// unconstrained. A system turn at `i >= k` is a mid-conversation message and
/// is refused only when it's immediately preceded by another system turn —
/// a system *run* after the head, which corresponds to no prefix any
/// provider hoisted (see `proposal.md`'s §7 correction: written literally
/// against "not index 0" the row would fail every lineage, since the leading
/// run legitimately occupies `messages[0]`).
fn check_system_placement(resolved: &[llm::Message]) -> Result<(), Refusal> {
    let head_len = llm::leading_system_run(resolved);
    for index in head_len..resolved.len() {
        if resolved[index].role == llm::Role::System
            && index > 0
            && resolved[index - 1].role == llm::Role::System
        {
            return Err(Refusal::MalformedTurns {
                message: format!(
                    "consecutive system turns at index {index} after the leading system run"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::MessageRef;

    fn lineage() -> Vec<Committed> {
        vec![Committed {
            id: "m0".into(),
            message: llm::Message::user("hi"),
        }]
    }

    #[test]
    fn a_faithful_echo_of_a_committed_message_is_not_a_content_mismatch() {
        // Regression: `content_matches` used to compare `stored`'s absent
        // `id` (by-value conversions always set `id: None`) against
        // `supplied`'s legitimately-present one, refusing every faithful
        // echo of a committed message for a reason that had nothing to do
        // with its content.
        let stored = &lineage()[0].message;
        let supplied = WireMessage::committed("m0", stored);
        assert!(content_matches(stored, &supplied));
    }

    #[test]
    fn a_genuinely_edited_echo_is_a_content_mismatch() {
        let stored = &lineage()[0].message;
        let mut supplied = WireMessage::committed("m0", stored);
        supplied.content[0] = crate::mapping::Content::Text {
            text: "edited".into(),
        };
        assert!(!content_matches(stored, &supplied));
    }

    fn matching_scaffold() -> ScaffoldPrint {
        scaffold::print_for(&[], lineage().iter().map(|c| &c.message), "anthropic", "m")
    }

    #[test]
    fn a_message_ref_alone_resolves_the_committed_prefix() {
        let scaffold = matching_scaffold();
        let supplied = [WireTurn::Ref(MessageRef { id: "m0".into() })];
        let sent = check(&supplied, &lineage(), &[], "anthropic", "m", &scaffold).unwrap();
        assert!(matches!(&sent[..], [SentTurn::Ref { id }] if id == "m0"));
    }

    #[test]
    fn an_unknown_id_is_ref_not_prefix() {
        let scaffold = matching_scaffold();
        let supplied = [WireTurn::Ref(MessageRef { id: "m99".into() })];
        let error = check(&supplied, &lineage(), &[], "anthropic", "m", &scaffold).unwrap_err();
        assert_eq!(error.kind(), "ref_not_prefix");
    }

    #[test]
    fn the_model_scaffold_component_is_checked_too() {
        // Unreachable within a single run (there's one client, hence one
        // model, per run) — exercised directly here rather than left
        // untested just because `crates/harness`'s own op layer never feeds
        // it two different models in one call.
        let scaffold_run = ScaffoldPrint {
            tools: scaffold::tools_hash(&[]),
            system: None,
            model: scaffold::model_hash("anthropic", "claude"),
        };
        let supplied = [WireTurn::Ref(MessageRef { id: "m0".into() })];
        let error = check(
            &supplied,
            &lineage(),
            &[],
            "anthropic",
            "a-different-model",
            &scaffold_run,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Refusal::ScaffoldMismatch { component: "model" }
        ));
    }
}
