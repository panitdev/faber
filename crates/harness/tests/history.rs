//! History as a real `Vec<llm::Message>`: the two-run splice, the round trip
//! that has to survive V8, request-option defaulting, and the frame-log
//! usage record — everything `proposal.md` §§2-4, 6, 8 promise but
//! `tests/identity.rs` doesn't already cover end to end.

mod support;

use std::sync::Arc;

use harness::frame::CoreEvent;
use harness::{HarnessRun, Seed};
use llm::{BlockStart, ContentBlock, Delta, Event, Message, StopReason, UsageDelta};
use support::{Recording, Scripted, drain_transcript, grant, input, seed, text_reply};

/// One thinking block (with its opaque signature) and one block this
/// contract doesn't model, so the completion the harness commits has both of
/// the shapes `read() -> slice -> send` has to preserve exactly.
fn thinking_and_unknown_reply(signature: &str, raw: &serde_json::Value) -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_t".into(),
            model: "test-model".into(),
            usage: UsageDelta::default(),
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::Thinking,
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::Thinking {
                content: "reasoning".into(),
            },
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::ThinkingSignature {
                content: signature.into(),
            },
        },
        Event::BlockStop { index: 0 },
        Event::BlockStart {
            index: 1,
            block: BlockStart::Unknown { raw: raw.clone() },
        },
        Event::BlockStop { index: 1 },
        Event::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            stop_details: None,
            usage: UsageDelta::default(),
        },
        Event::MessageStop,
    ]
}

/// T1: run one commits a lineage; run two — seeded from what run one
/// committed, since nothing survives between runs — slices it and splices a
/// replacement tail. What this proves is render purity plus splice
/// correctness: Core always renders an id-bearing turn from *stored*
/// content, so the referenced prefix's rendered bytes are identical
/// regardless of which run produced them. It does not by itself prove the
/// round trip through JS was faithful — `round_trip_preserves_what_v8_cannot`
/// (T2) is what proves that.
#[test]
fn a_referenced_prefix_renders_byte_identical_across_runs() {
    const STREAM_AND_COMMIT: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
    for await (const event of call) { /* drain */ }
    await ctx.commit(call);
  }
};
"#;

    let client1 = Arc::new(Recording::new(text_reply("hello back")));
    let mut run1 = HarnessRun::start(
        STREAM_AND_COMMIT.to_string(),
        input("hi"),
        grant(client1.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run1);
    support::finished(run1, "run one must finish cleanly");

    let run1_rendered = client1.rendered();
    assert_eq!(run1_rendered.len(), 1);
    let run1_region = run1_rendered[0]
        .prefix
        .regions
        .get("messages")
        .expect("run one must have rendered a messages region")
        .clone();

    // What run one actually committed: the "hi" input plus its reply. Run
    // two is seeded with this directly — no cross-run persistence exists yet
    // (`crates/api`'s spine), so the test plays that role by hand.
    let committed = vec![
        Message::user("hi"),
        Message::assistant(vec![ContentBlock::Text {
            text: "hello back".into(),
        }]),
    ];

    const CUT_AND_SPLICE: &str = r#"
export default {
  execute: (ctx, input) => {
    const h = ctx.history.read();
    return ctx.llm.stream({
      messages: [
        { id: h[0].id },
        { role: "assistant", content: [{ type: "text", text: "replaced" }] },
        ...input,
      ],
    });
  }
};
"#;

    let client2 = Arc::new(Recording::new(text_reply("run two reply")));
    let mut run2 = HarnessRun::start(
        CUT_AND_SPLICE.to_string(),
        input("hi again"),
        grant(client2.clone()),
        seed("anthropic", "test-model", committed),
    );
    let _ = drain_transcript(&mut run2);
    support::finished(run2, "run two must finish cleanly");

    let run2_rendered = client2.rendered();
    assert_eq!(run2_rendered.len(), 1);
    let run2_region = run2_rendered[0]
        .prefix
        .regions
        .get("messages")
        .expect("run two must have rendered a messages region");

    assert!(
        run2_region.starts_with(&run1_region),
        "the referenced prefix's rendered bytes must be identical across runs;\n\
         run one:  {}\n\
         run two:  {}",
        String::from_utf8_lossy(&run1_region),
        String::from_utf8_lossy(run2_region),
    );
}

/// T2: the round trip that has to survive V8. `history.read()` hands the
/// harness a JS copy of the committed message; sending it back *by value,
/// with its id still attached* is only accepted if the copy matches what
/// Core stored — `content_matches` normalizes both sides precisely because
/// this trip is where `1.0` can become `1` inside `Unknown.raw`. What
/// actually gets sent, either way, is Core's own stored content — proven
/// here by asserting the opaque signature appears verbatim in the rendered
/// bytes.
#[test]
fn round_trip_preserves_what_v8_cannot() {
    let raw = serde_json::json!({
        "b": 1,
        "a": 1.0,
        "nested": {"z": [1, 2, {"deep": true}], "emptyObj": {}, "emptyArr": []},
        "unicode": "caf\u{e9}",
    });
    let signature = "opaque-signature-xyz";

    const RESEND_HISTORY_BY_VALUE: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call1 = ctx.llm.stream({ messages: [...input] });
    for await (const event of call1) { /* drain */ }
    await ctx.commit(call1);

    const history = ctx.history.read();
    // Sent back *by value*, id attached — the round trip under test. If V8
    // had mangled anything `content_matches` doesn't normalize away, this
    // throws `ref_content_mismatch` and the test fails below.
    const call2 = ctx.llm.stream({ messages: [history[0], history[1], ...input] });
    for await (const event of call2) { /* drain */ }
    yield { type: "unknown", raw: { ok: true } };
  }
};
"#;

    let client = Arc::new(Recording::sequence(vec![
        thinking_and_unknown_reply(signature, &raw),
        text_reply("ack"),
    ]));
    let mut run = HarnessRun::start(
        RESEND_HISTORY_BY_VALUE.to_string(),
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "a faithful round trip must not be refused");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["raw"]["ok"], true);

    let rendered = client.rendered();
    assert_eq!(rendered.len(), 2);
    let second_region = rendered[1]
        .prefix
        .regions
        .get("messages")
        .expect("the second call must have rendered a messages region");
    let region_text = String::from_utf8_lossy(second_region);
    assert!(
        region_text.contains(signature),
        "the thinking signature must survive verbatim into the wire bytes Core actually sends, \
         regardless of what the harness's own JS copy of it looked like: {region_text}"
    );
}

/// T4: every option field but `messages` defaults to the committed baseline
/// (§4), and a deliberate off-lineage call that's never committed does not
/// leak into the next default-path call (§4's anchoring claim).
#[test]
fn options_default_to_the_committed_baseline_and_off_lineage_calls_do_not_leak() {
    const FIVE_CALLS: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const a = ctx.llm.stream({
      effort: "high",
      maxTokens: 500,
      sampling: { temperature: 0.5 },
      extra: { vendorFlag: true },
      messages: [...ctx.history.read(), ...input],
    });
    for await (const e of a) {}
    await ctx.commit(a);

    const b = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
    for await (const e of b) {}
    await ctx.commit(b);

    const c = ctx.llm.stream({ effort: "low", messages: [...ctx.history.read(), ...input] });
    for await (const e of c) {}
    await ctx.commit(c);

    // Off-lineage: streamed but never committed.
    const d = ctx.llm.stream({ effort: "medium", messages: [...ctx.history.read(), ...input] });
    for await (const e of d) {}

    const e2 = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
    for await (const e of e2) {}

    yield { type: "unknown", raw: { ok: true } };
  }
};
"#;

    let client = Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("b"),
        text_reply("c"),
        text_reply("d"),
        text_reply("e"),
    ]));
    let mut run = HarnessRun::start(
        FIVE_CALLS.to_string(),
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 5);

    assert_eq!(requests[0].effort, Some(llm::Effort::High));
    assert_eq!(requests[0].max_tokens, 500);
    assert_eq!(requests[0].sampling.temperature, Some(0.5));
    assert_eq!(
        requests[0].extra.get("vendorFlag"),
        Some(&serde_json::json!(true))
    );

    // Default path: inherits everything from the committed call A, `extra`
    // and `sampling` included — both are new since the span model, and both
    // were silently dropped before this refactor (`extra` was never even
    // wired to `llm::Request`).
    assert_eq!(requests[1].effort, Some(llm::Effort::High));
    assert_eq!(requests[1].max_tokens, 500);
    assert_eq!(requests[1].sampling.temperature, Some(0.5));
    assert_eq!(
        requests[1].extra.get("vendorFlag"),
        Some(&serde_json::json!(true))
    );

    // Explicit override on effort only; maxTokens/sampling/extra all still
    // inherited.
    assert_eq!(requests[2].effort, Some(llm::Effort::Low));
    assert_eq!(requests[2].max_tokens, 500);
    assert_eq!(requests[2].sampling.temperature, Some(0.5));

    // Off-lineage, never committed.
    assert_eq!(requests[3].effort, Some(llm::Effort::Medium));

    // Default path again: must reflect C's commit (low), not D's uncommitted
    // medium — a scratch call cannot poison the next default-path call.
    assert_eq!(requests[4].effort, Some(llm::Effort::Low));
    assert_eq!(requests[4].max_tokens, 500);
    assert_eq!(requests[4].sampling.temperature, Some(0.5));
}

/// The caller's own reasoning selection (`Grant::reasoning`) is the default
/// for every call in the run — including one seeded from a turn that
/// committed a different one, because the user can move the knob between two
/// turns — while an explicit value on a call still wins.
#[test]
fn a_granted_reasoning_selection_defaults_every_call_and_yields_to_an_explicit_one() {
    const TWO_CALLS: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const a = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
    for await (const e of a) {}
    await ctx.commit(a);

    const b = ctx.llm.stream({ effort: "low", messages: [...ctx.history.read(), ...input] });
    for await (const e of b) {}
  }
};
"#;

    let client = Arc::new(Scripted::sequence(vec![text_reply("a"), text_reply("b")]));

    // What the previous turn ran at. Whatever it was, this run was told
    // something else.
    let mut seeded = seed("anthropic", "test-model", vec![]);
    seeded.options.effort = Some(llm::Effort::Max);
    seeded.options.thinking = Some(llm::Thinking::Disabled);

    let mut run = HarnessRun::start(
        TWO_CALLS.to_string(),
        input("hi"),
        support::grant_reasoning(
            client.clone(),
            harness::Reasoning {
                thinking: Some(llm::Thinking::Adaptive {
                    display: llm::ThinkingDisplay::Summarized,
                }),
                effort: Some(llm::Effort::High),
            },
        ),
        seeded,
    );
    let _ = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 2);

    // The default path takes the selection, not the seeded baseline.
    assert_eq!(requests[0].effort, Some(llm::Effort::High));
    assert!(matches!(
        requests[0].thinking,
        Some(llm::Thinking::Adaptive { .. })
    ));

    // An explicit override is still a decision the workflow gets to make; it
    // only replaces the field it names.
    assert_eq!(requests[1].effort, Some(llm::Effort::Low));
    assert!(matches!(
        requests[1].thinking,
        Some(llm::Thinking::Adaptive { .. })
    ));
}

/// A caller with a selection of "leave it to the provider" says so by
/// granting empty fields — which has to clear what an earlier turn committed
/// rather than read as "no opinion", or turning the knob back off would never
/// take effect.
#[test]
fn a_granted_selection_of_nothing_clears_the_seeded_baseline() {
    const ONE_CALL: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const a = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
    for await (const e of a) {}
  }
};
"#;

    let client = Arc::new(Scripted::new(text_reply("a")));
    let mut seeded = seed("anthropic", "test-model", vec![]);
    seeded.options.effort = Some(llm::Effort::Max);

    let mut run = HarnessRun::start(
        ONE_CALL.to_string(),
        input("hi"),
        support::grant_reasoning(client.clone(), harness::Reasoning::default()),
        seeded,
    );
    let _ = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].effort, None);
    assert!(requests[0].thinking.is_none());
}

/// A model's advanced options (`llm::AdvancedOptions`, granted rather than
/// requested — same reasoning as `reasoning_history`) are a floor under every
/// call: present with nothing else asked for, and still present — merged, not
/// replaced — when a call supplies its own `extra`.
#[test]
fn advanced_options_from_the_grant_underlie_every_calls_extra() {
    const TWO_CALLS: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const a = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
    for await (const e of a) {}
    await ctx.commit(a);

    const b = ctx.llm.stream({
      extra: { vendorFlag: true },
      messages: [...ctx.history.read(), ...input],
    });
    for await (const e of b) {}

    yield { type: "unknown", raw: { ok: true } };
  }
};
"#;

    let client = Arc::new(Scripted::sequence(vec![text_reply("a"), text_reply("b")]));
    let mut g = grant(client.clone());
    g.advanced_options = llm::AdvancedOptions {
        reasoning_split: true,
        extra: serde_json::json!({ "top_k": 5 })
            .as_object()
            .unwrap()
            .clone(),
    };
    let mut run = HarnessRun::start(TWO_CALLS.to_string(), input("hi"), g, Seed::default());
    let _ = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 2);

    assert_eq!(
        requests[0].extra.get("reasoning_split"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(requests[0].extra.get("top_k"), Some(&serde_json::json!(5)));

    // The call's own `extra` adds a key rather than wiping the grant's floor.
    assert_eq!(
        requests[1].extra.get("reasoning_split"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(requests[1].extra.get("top_k"), Some(&serde_json::json!(5)));
    assert_eq!(
        requests[1].extra.get("vendorFlag"),
        Some(&serde_json::json!(true))
    );
}

/// T5: before anything is committed, `committedRequest()` is exactly the
/// granted tool set and nothing else.
#[test]
fn turn_one_committed_request_is_just_the_granted_tools() {
    const YIELD_COMMITTED_REQUEST: &str = r#"
export default {
  execute: async function* (ctx) {
    yield { type: "unknown", raw: ctx.committedRequest() };
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut g = grant(client);
    g.tools = vec![llm::ToolDef {
        name: "known".into(),
        description: "a tool that exists".into(),
        input_schema: serde_json::json!({}),
    }];

    let mut run = HarnessRun::start(
        YIELD_COMMITTED_REQUEST.to_string(),
        input("hi"),
        g,
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    assert_eq!(events.len(), 1);
    let raw = &events[0]["raw"];
    assert_eq!(
        raw["tools"],
        serde_json::json!([{"name": "known", "description": "a tool that exists", "inputSchema": {}}])
    );
    assert!(raw.get("maxTokens").is_none());
    assert!(raw.get("toolChoice").is_none());
    assert!(raw.get("thinking").is_none());
    assert!(raw.get("effort").is_none());
    assert!(raw.get("sampling").is_none());
    assert!(raw.get("stopSequences").is_none());
    assert!(raw.get("extra").is_none());
}

/// T6: `read()` and `tools.available` are both frozen — mutating either
/// throws rather than silently succeeding and lying about what's stored.
#[test]
fn read_and_available_tools_are_both_frozen() {
    const MUTATE_BOTH: &str = r#"
export default {
  execute: async function* (ctx, input) {
    // Turn one: commit something, so `read()` returns non-empty messages —
    // an empty array is trivially frozen; a message's own nested `content`
    // array is the case `deepFreeze`'s recursion exists for.
    const call = ctx.llm.stream({ messages: [...input] });
    for await (const event of call) { /* drain */ }
    await ctx.commit(call);

    let historyArrayMutationThrew = false;
    try { ctx.history.read().push({}); } catch (e) { historyArrayMutationThrew = true; }

    let historyContentMutationThrew = false;
    try { ctx.history.read()[0].content.push({}); } catch (e) { historyContentMutationThrew = true; }

    let toolsMutationThrew = false;
    try { ctx.tools.available.push({}); } catch (e) { toolsMutationThrew = true; }

    yield {
      type: "unknown",
      raw: { historyArrayMutationThrew, historyContentMutationThrew, toolsMutationThrew },
    };
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("reply")));
    let mut run = HarnessRun::start(
        MUTATE_BOTH.to_string(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["raw"]["historyArrayMutationThrew"], true);
    assert_eq!(events[0]["raw"]["historyContentMutationThrew"], true);
    assert_eq!(events[0]["raw"]["toolsMutationThrew"], true);
}

/// T7: a `ModelUsage` frame is recorded exactly once per model frame, and
/// agrees with folding that frame's own `ModelEvent`s through `Accumulator`
/// — on both the clean path and the failed one, since a truncated call still
/// burned tokens (`harness-events.md` §8).
#[test]
fn model_usage_is_recorded_on_both_the_clean_and_failed_path() {
    const STREAM_ONLY: &str = r#"
export default {
  execute: async function* (ctx, input) {
    try {
      for await (const event of ctx.llm.stream({ messages: [...input] })) { /* drain */ }
    } catch (e) { /* the failed-path test expects this */ }
  }
};
"#;

    // Clean path.
    {
        let client = Arc::new(Scripted::new(text_reply("usage check")));
        let mut run = HarnessRun::start(
            STREAM_ONLY.to_string(),
            input("hi"),
            grant(client),
            Seed::default(),
        );
        let _ = drain_transcript(&mut run);
        let frames = support::finished(run, "run must finish cleanly").frames;
        assert_usage_matches_the_fold(&frames);
    }

    // Failed path: the stream itself errors mid-message.
    {
        let client: Arc<dyn llm::ModelClient> = Arc::new(support::Failing);
        let mut run = HarnessRun::start(
            STREAM_ONLY.to_string(),
            input("hi"),
            grant(client),
            Seed::default(),
        );
        let _ = drain_transcript(&mut run);
        let frames = support::finished(run, "run must finish even though the call failed").frames;
        assert_usage_matches_the_fold(&frames);
    }
}

fn assert_usage_matches_the_fold(frames: &[CoreEvent]) {
    let frame_id = frames
        .iter()
        .find_map(|frame| match frame {
            CoreEvent::FrameStart { frame, .. } => Some(frame.clone()),
            _ => None,
        })
        .expect("a model frame must have started");

    let usages: Vec<llm::Usage> = frames
        .iter()
        .filter_map(|frame| match frame {
            CoreEvent::ModelUsage { frame: id, usage } if *id == frame_id => Some(*usage),
            _ => None,
        })
        .collect();
    assert_eq!(usages.len(), 1, "exactly one ModelUsage per model frame");

    let mut accumulator = llm::Accumulator::new();
    for frame in frames {
        if let CoreEvent::ModelEvent { frame: id, event } = frame
            && *id == frame_id
        {
            accumulator.push(event);
        }
    }
    let completion = accumulator
        .finish()
        .expect("both scripts leave the accumulator in a foldable state");

    assert_eq!(usages[0].input_tokens, completion.usage.input_tokens);
    assert_eq!(usages[0].output_tokens, completion.usage.output_tokens);
    assert_eq!(
        usages[0].cache_read_input_tokens,
        completion.usage.cache_read_input_tokens
    );
}
