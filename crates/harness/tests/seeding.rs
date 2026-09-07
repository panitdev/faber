//! What a run leaves behind for the next one: the committed lineage, the
//! request bytes, and the frame that produced them.
//!
//! `tests/history.rs` covers the cross-run splice by seeding run two *by
//! hand* — it hardcodes what run one committed, because at the time nothing
//! reported it. These tests close that loop: run two is seeded from run one's
//! own [`RunOutcome`], which is exactly what `crates/api` does through the
//! spine (`history-abstract.md` H7).

mod support;

use std::sync::Arc;

use harness::frame::{CoreEvent, FrameDetail};
use harness::{HarnessRun, Seed};
use llm::{ContentBlock, Message, Role, Turn};
use support::{Recording, drain_transcript, grant, input, text_reply};

/// Flattens a request's turns to `(role, text)`, for asserting on what a run
/// actually sent without depending on any provider's rendering.
fn turns_as_text(messages: &[Turn]) -> Vec<(Role, String)> {
    messages
        .iter()
        .filter_map(|turn| match turn {
            Turn::Value(message) => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                Some((message.role, text))
            }
            Turn::Span(_) => None,
        })
        .collect()
}

/// The seeding round trip, end to end and with no hand-written lineage in the
/// middle: run one commits, its `RunOutcome` seeds run two, and run two's
/// request carries run one's turns ahead of the new input.
///
/// This is the property a conversation depends on. Without it every turn
/// starts from an empty history and the model cannot see what it just said.
#[test]
fn a_run_outcome_seeds_the_next_run_with_what_it_committed() {
    let client1 = Arc::new(Recording::new(text_reply("nice to meet you, Ada")));
    let mut run1 = HarnessRun::start(
        harness::CONVERSATIONAL.to_string(),
        input("my name is Ada"),
        grant(client1.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run1);
    let outcome1 = support::finished(run1, "run one must finish cleanly");

    let committed = turns_as_text(
        &outcome1
            .committed
            .messages
            .iter()
            .cloned()
            .map(Turn::Value)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        committed.len(),
        3,
        "a committed run's lineage is the default prompt plus its request plus the completion it produced"
    );
    assert_eq!(committed[0].0, Role::System);
    assert!(
        committed[0].1.contains("Faber"),
        "the leading turn is the default prompt: {}",
        committed[0].1
    );
    assert_eq!(
        &committed[1..],
        &[
            (Role::User, "my name is Ada".to_string()),
            (Role::Assistant, "nice to meet you, Ada".to_string()),
        ],
    );
    assert!(
        outcome1.committed_frame.is_some(),
        "a commit has to name the frame it came from, or nothing can put it on a spine"
    );

    // The whole point: run two is handed run one's own report, not a fixture.
    let client2 = Arc::new(Recording::new(text_reply("Ada")));
    let mut run2 = HarnessRun::start(
        harness::CONVERSATIONAL.to_string(),
        input("what is my name?"),
        grant(client2.clone()),
        outcome1.committed,
    );
    let _ = drain_transcript(&mut run2);
    support::finished(run2, "run two must finish cleanly");

    let sent = client2.requests_seen();
    assert_eq!(sent.len(), 1);
    let turn_two = turns_as_text(&sent[0].messages);
    assert_eq!(
        turn_two.len(),
        4,
        "turn two must see the default prompt plus turn one's conversation ahead of the new input"
    );
    assert_eq!(turn_two[0].0, Role::System);
    assert_eq!(
        &turn_two[1..],
        &[
            (Role::User, "my name is Ada".to_string()),
            (Role::Assistant, "nice to meet you, Ada".to_string()),
            (Role::User, "what is my name?".to_string()),
        ],
    );
}

/// A harness that never commits leaves the lineage exactly as it was seeded,
/// and reports no frame — so a caller writing a spine has nothing to write,
/// which is the correct outcome rather than a silent guess at one.
///
/// This is what `identity.js` does, and why it is not the harness the API
/// runs (`types.d.ts`: history does not auto-advance in this substrate).
#[test]
fn a_run_that_never_commits_reports_no_position() {
    let client = Arc::new(Recording::new(text_reply("hello back")));
    let mut run = HarnessRun::start(
        harness::IDENTITY.to_string(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run);
    let outcome = support::finished(run, "run must finish cleanly");

    assert!(outcome.committed.messages.is_empty());
    assert!(outcome.committed_frame.is_none());
}

/// The frame log carries the request bytes as rendered, and they are the ones
/// that were actually sent — `exchange.request_blob_digest` has no other
/// source, and J1 requires the record to come from the op rather than from
/// anything the harness says about itself.
#[test]
fn the_frame_log_records_the_bytes_that_were_sent() {
    let client = Arc::new(Recording::new(text_reply("hello back")));
    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_string(),
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run);
    let outcome = support::finished(run, "run must finish cleanly");

    let model_frame = outcome
        .frames
        .iter()
        .find_map(|event| match event {
            CoreEvent::FrameStart {
                frame,
                detail: FrameDetail::Model { .. },
                ..
            } => Some(frame.clone()),
            _ => None,
        })
        .expect("a run that called the model must open a model frame");

    let recorded = outcome
        .frames
        .iter()
        .find_map(|event| match event {
            CoreEvent::ModelRequest { frame, body } if *frame == model_frame => Some(body.clone()),
            _ => None,
        })
        .expect("every model frame must record its request bytes");

    let rendered = client.rendered();
    assert_eq!(rendered.len(), 1);
    assert_eq!(
        recorded, rendered[0].body,
        "the logged bytes must be the rendered bytes, not a re-serialization of them"
    );
    assert!(!recorded.is_empty());
}

/// A lineage survives storage.
///
/// `crates/api` serializes a `Seed` into a blob and reads it back a turn
/// later, so anything the type cannot round-trip is context silently lost
/// between turns. The two blocks that matter are the ones the content model
/// exists to protect: `Thinking.signature` is opaque and must go back to the
/// model verbatim, and `Unknown` exists precisely so a block that arrived can
/// be sent back.
#[test]
fn a_seed_round_trips_through_storage() {
    let raw = serde_json::json!({
        "type": "server_tool_use",
        "id": "srvtoolu_1",
        "nested": { "b": 2, "a": 1 },
    });

    let seed = Seed {
        provider: "anthropic".into(),
        model: "test-model".into(),
        messages: vec![
            Message::user("hi"),
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "reasoning".into(),
                    signature: Some("sig-opaque-value".into()),
                },
                ContentBlock::Unknown { raw: raw.clone() },
                ContentBlock::Text {
                    text: "hello back".into(),
                },
            ]),
        ],
        options: Default::default(),
    };

    let bytes = serde_json::to_vec(&seed).expect("a seed must encode");
    let restored: Seed = serde_json::from_slice(&bytes).expect("a seed must decode");

    assert_eq!(restored.provider, "anthropic");
    assert_eq!(restored.model, "test-model");
    assert_eq!(restored.messages.len(), 2);

    let blocks = &restored.messages[1].content;
    assert_eq!(blocks.len(), 3);

    match &blocks[0] {
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            assert_eq!(thinking, "reasoning");
            assert_eq!(
                signature.as_deref(),
                Some("sig-opaque-value"),
                "a dropped signature makes the block unreturnable to the model"
            );
        }
        other => panic!("expected a thinking block, got {other:?}"),
    }

    match &blocks[1] {
        ContentBlock::Unknown { raw: restored_raw } => {
            assert_eq!(*restored_raw, raw, "an unknown block must survive whole");
        }
        other => panic!("expected an unknown block, got {other:?}"),
    }
}
