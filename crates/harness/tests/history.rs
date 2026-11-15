//! History as a real `Vec<llm::Message>`: the two-run splice, the by-value
//! round trip, request-option defaulting, and the frame-log usage record —
//! everything `proposal.md` §§2-4, 6, 8 promise but `tests/identity.rs`
//! doesn't already cover end to end.

mod support;

use std::sync::Arc;

use harness::frame::CoreEvent;
use harness::mapping::{CommitOptions, Effort, LLMRequest, Role, Sampling, WireTurn};
use harness::{Context, Error, HarnessRun, Seed};
use llm::{BlockStart, ContentBlock, Delta, Event, Message, StopReason, UsageDelta};
use serde_json::json;
use support::{
    Recording, Scripted, Wire, drain, drain_transcript, grant, history_and, input, seed, text_reply,
};

/// Streams one call for `request`, keeping nothing, and commits it.
async fn stream_and_commit(ctx: &Context, request: LLMRequest) -> Result<(), Error> {
    let call = ctx.stream(request)?;
    drain(&call).await?;
    ctx.commit(&call, CommitOptions::default()).await
}

/// Streams one call for `request`, keeping nothing, and commits nothing.
async fn stream_only(ctx: &Context, request: LLMRequest) -> Result<(), Error> {
    drain(&ctx.stream(request)?).await
}

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
/// correctness: an id-bearing turn is always rendered from *stored* content,
/// so the referenced prefix's rendered bytes are identical regardless of
/// which run produced them.
#[tokio::test]
async fn a_referenced_prefix_renders_byte_identical_across_runs() {
    async fn commit_one(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        stream_and_commit(&ctx, LLMRequest::new(history_and(&ctx, &input))).await
    }

    let client1 = Arc::new(Recording::new(text_reply("hello back")));
    let mut run1 = HarnessRun::start_with(
        commit_one,
        input("hi"),
        grant(client1.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run1).await;
    support::finished(run1, "run one must finish cleanly").await;

    let run1_rendered = client1.rendered();
    assert_eq!(run1_rendered.len(), 1);
    let run1_region = run1_rendered[0]
        .prefix
        .regions
        .get("messages")
        .expect("run one must have rendered a messages region")
        .clone();

    // What run one actually committed: the "hi" input plus its reply. Run
    // two is seeded with this directly; the test plays the part of the
    // caller's storage by hand.
    let committed = vec![
        Message::user("hi"),
        Message::assistant(vec![ContentBlock::Text {
            text: "hello back".into(),
        }]),
    ];

    async fn cut_and_splice(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let history = ctx.history();
        let first = history[0].id.clone().expect("history carries ids");
        let mut messages = vec![
            WireTurn::reference(first),
            WireTurn::from(Wire::text(Role::Assistant, "replaced")),
        ];
        messages.extend(input.into_iter().map(WireTurn::from));
        stream_only(
            &ctx,
            LLMRequest {
                messages,
                ..LLMRequest::default()
            },
        )
        .await
    }

    let client2 = Arc::new(Recording::new(text_reply("run two reply")));
    let mut run2 = HarnessRun::start_with(
        cut_and_splice,
        input("hi again"),
        grant(client2.clone()),
        seed("anthropic", "test-model", committed),
    );
    let _ = drain_transcript(&mut run2).await;
    support::finished(run2, "run two must finish cleanly").await;

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

/// T2: `history()` hands the loop a copy of the committed message; sending
/// it back *by value, with its id still attached* is only accepted if the
/// copy matches what is stored. What actually gets sent, either way, is the
/// stored content — proven here by asserting the opaque signature appears
/// verbatim in the rendered bytes.
#[tokio::test]
async fn an_id_bearing_copy_of_history_is_accepted_and_sent_from_stored_content() {
    let raw = serde_json::json!({
        "b": 1,
        "a": 1.0,
        "nested": {"z": [1, 2, {"deep": true}], "emptyObj": {}, "emptyArr": []},
        "unicode": "caf\u{e9}",
    });
    let signature = "opaque-signature-xyz";

    async fn resend_history_by_value(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        stream_and_commit(&ctx, LLMRequest::new(input.clone())).await?;

        // Sent back *by value*, id attached — the round trip under test. A
        // copy that differed from what is stored would be refused as
        // `ref_content_mismatch` and fail the run.
        stream_only(&ctx, LLMRequest::new(history_and(&ctx, &input))).await?;
        ctx.emit(json!({ "type": "unknown", "raw": { "ok": true } }));
        Ok(())
    }

    let client = Arc::new(Recording::sequence(vec![
        thinking_and_unknown_reply(signature, &raw),
        text_reply("ack"),
    ]));
    let mut run = HarnessRun::start_with(
        resend_history_by_value,
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let events = drain_transcript(&mut run).await;
    support::finished(run, "a faithful round trip must not be refused").await;

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
        "the thinking signature must survive verbatim into the wire bytes actually sent: \
         {region_text}"
    );
}

/// T4: every option field but `messages` defaults to the committed baseline
/// (§4), and a deliberate off-lineage call that's never committed does not
/// leak into the next default-path call (§4's anchoring claim).
#[tokio::test]
async fn options_default_to_the_committed_baseline_and_off_lineage_calls_do_not_leak() {
    async fn five_calls(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let request = |ctx: &Context| LLMRequest::new(history_and(ctx, &input));

        stream_and_commit(
            &ctx,
            LLMRequest {
                effort: Some(Effort::High),
                max_tokens: Some(500),
                sampling: Some(Sampling {
                    temperature: Some(0.5),
                    ..Sampling::default()
                }),
                extra: json!({ "vendorFlag": true }).as_object().cloned(),
                ..request(&ctx)
            },
        )
        .await?;

        stream_and_commit(&ctx, request(&ctx)).await?;

        stream_and_commit(
            &ctx,
            LLMRequest {
                effort: Some(Effort::Low),
                ..request(&ctx)
            },
        )
        .await?;

        // Off-lineage: streamed but never committed.
        stream_only(
            &ctx,
            LLMRequest {
                effort: Some(Effort::Medium),
                ..request(&ctx)
            },
        )
        .await?;

        stream_only(&ctx, request(&ctx)).await?;

        ctx.emit(json!({ "type": "unknown", "raw": { "ok": true } }));
        Ok(())
    }

    let client = Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("b"),
        text_reply("c"),
        text_reply("d"),
        text_reply("e"),
    ]));
    let mut run = HarnessRun::start_with(
        five_calls,
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

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
    // and `sampling` included.
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
#[tokio::test]
async fn a_granted_reasoning_selection_defaults_every_call_and_yields_to_an_explicit_one() {
    async fn two_calls(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        stream_and_commit(&ctx, LLMRequest::new(history_and(&ctx, &input))).await?;
        stream_only(
            &ctx,
            LLMRequest {
                effort: Some(Effort::Low),
                ..LLMRequest::new(history_and(&ctx, &input))
            },
        )
        .await
    }

    let client = Arc::new(Scripted::sequence(vec![text_reply("a"), text_reply("b")]));

    // What the previous turn ran at. Whatever it was, this run was told
    // something else.
    let mut seeded = seed("anthropic", "test-model", vec![]);
    seeded.options.effort = Some(llm::Effort::Max);
    seeded.options.thinking = Some(llm::Thinking::Disabled);

    let mut run = HarnessRun::start_with(
        two_calls,
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
    let _ = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 2);

    // The default path takes the selection, not the seeded baseline.
    assert_eq!(requests[0].effort, Some(llm::Effort::High));
    assert!(matches!(
        requests[0].thinking,
        Some(llm::Thinking::Adaptive { .. })
    ));

    // An explicit override is still a decision a call gets to make; it only
    // replaces the field it names.
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
#[tokio::test]
async fn a_granted_selection_of_nothing_clears_the_seeded_baseline() {
    async fn one_call(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        stream_only(&ctx, LLMRequest::new(history_and(&ctx, &input))).await
    }

    let client = Arc::new(Scripted::new(text_reply("a")));
    let mut seeded = seed("anthropic", "test-model", vec![]);
    seeded.options.effort = Some(llm::Effort::Max);

    let mut run = HarnessRun::start_with(
        one_call,
        input("hi"),
        support::grant_reasoning(client.clone(), harness::Reasoning::default()),
        seeded,
    );
    let _ = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].effort, None);
    assert!(requests[0].thinking.is_none());
}

/// A model's advanced options (`llm::AdvancedOptions`, granted rather than
/// requested — same reasoning as `reasoning_history`) are a floor under every
/// call: present with nothing else asked for, and still present — merged, not
/// replaced — when a call supplies its own `extra`.
#[tokio::test]
async fn advanced_options_from_the_grant_underlie_every_calls_extra() {
    async fn two_calls(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        stream_and_commit(&ctx, LLMRequest::new(history_and(&ctx, &input))).await?;
        stream_only(
            &ctx,
            LLMRequest {
                extra: json!({ "vendorFlag": true }).as_object().cloned(),
                ..LLMRequest::new(history_and(&ctx, &input))
            },
        )
        .await?;
        ctx.emit(json!({ "type": "unknown", "raw": { "ok": true } }));
        Ok(())
    }

    let client = Arc::new(Scripted::sequence(vec![text_reply("a"), text_reply("b")]));
    let mut g = grant(client.clone());
    g.advanced_options = llm::AdvancedOptions {
        reasoning_split: true,
        extra: serde_json::json!({ "top_k": 5 })
            .as_object()
            .unwrap()
            .clone(),
    };
    let mut run = HarnessRun::start_with(two_calls, input("hi"), g, Seed::default());
    let _ = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

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

/// T5: before anything is committed, the committed request is exactly the
/// granted tool set and nothing else.
#[tokio::test]
async fn turn_one_committed_request_is_just_the_granted_tools() {
    async fn report_committed_request(ctx: Context, _input: Vec<Wire>) -> Result<(), Error> {
        let raw = serde_json::to_value(ctx.committed_request()).expect("options serialize");
        ctx.emit(json!({ "type": "unknown", "raw": raw }));
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut g = grant(client);
    g.tools = vec![llm::ToolDef {
        name: "known".into(),
        description: "a tool that exists".into(),
        input_schema: serde_json::json!({}),
    }];

    let mut run = HarnessRun::start_with(report_committed_request, input("hi"), g, Seed::default());
    let events = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

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

/// T6: `history()` hands out copies — changing one changes nothing that is
/// stored, so a later read is still the committed lineage.
#[tokio::test]
async fn editing_a_copy_of_history_does_not_change_what_is_stored() {
    async fn edit_a_copy(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        stream_and_commit(&ctx, LLMRequest::new(input)).await?;

        let mut copy = ctx.history();
        copy[0].content.clear();
        copy.pop();

        let stored = ctx.history();
        ctx.emit(json!({
            "type": "unknown",
            "raw": { "length": stored.len(), "firstBlocks": stored[0].content.len() },
        }));
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("reply")));
    let mut run = HarnessRun::start_with(edit_a_copy, input("hi"), grant(client), Seed::default());
    let events = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["raw"]["length"], 2);
    assert_eq!(events[0]["raw"]["firstBlocks"], 1);
}

/// T7: a `ModelUsage` frame is recorded exactly once per model frame, and
/// agrees with folding that frame's own `ModelEvent`s through `Accumulator`
/// — on both the clean path and the failed one, since a truncated call still
/// burned tokens (`harness-events.md` §8).
#[tokio::test]
async fn model_usage_is_recorded_on_both_the_clean_and_failed_path() {
    async fn tolerant(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        // The failed-path case expects the stream to break.
        let _ = stream_only(&ctx, LLMRequest::new(input)).await;
        Ok(())
    }

    // Clean path.
    {
        let client = Arc::new(Scripted::new(text_reply("usage check")));
        let mut run = HarnessRun::start_with(tolerant, input("hi"), grant(client), Seed::default());
        let _ = drain_transcript(&mut run).await;
        let frames = support::finished(run, "run must finish cleanly")
            .await
            .frames;
        assert_usage_matches_the_fold(&frames);
    }

    // Failed path: the stream itself errors mid-message.
    {
        let client: Arc<dyn llm::ModelClient> = Arc::new(support::Failing);
        let mut run = HarnessRun::start_with(tolerant, input("hi"), grant(client), Seed::default());
        let _ = drain_transcript(&mut run).await;
        let frames = support::finished(run, "run must finish even though the call failed")
            .await
            .frames;
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
        if let CoreEvent::ModelEvent { frame: id, event, .. } = frame
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
