//! Compaction through the conversational loop: what is summarized, what is
//! kept, what the lineage becomes once the turn commits, and when it does
//! not happen at all.

mod support;

use std::sync::Arc;

use harness::{Compaction, Error, HarnessRun, RunOutcome, Seed};
use llm::{BlockStart, ContentBlock, Delta, Event, Message, Role, StopReason, UsageDelta};
use serde_json::Value;
use support::{Scripted, grant, input, text_reply};

fn system(text: &str) -> Message {
    Message {
        role: Role::System,
        content: vec![ContentBlock::Text { text: text.into() }],
    }
}

fn texts(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A lineage one exchange long, as a prior run committed it.
fn earlier(context_tokens: Option<u64>) -> Seed {
    Seed {
        provider: "scripted".into(),
        model: "test-model".into(),
        messages: vec![
            system("sys"),
            Message::user("build the thing"),
            Message::assistant(vec![ContentBlock::Text {
                text: "built it".into(),
            }]),
        ],
        options: Default::default(),
        context_tokens,
    }
}

fn compacting(window: Option<u64>, requested: bool) -> Compaction {
    Compaction {
        context_window: window,
        requested,
        instructions: None,
    }
}

/// A tool call whose usage says the context is `input_tokens` large.
fn tool_call_measuring(input_tokens: u64) -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_tool".into(),
            model: "test-model".into(),
            usage: UsageDelta {
                input_tokens: Some(input_tokens),
                ..UsageDelta::default()
            },
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::ToolUse {
                id: "call_1".into(),
                name: "read".into(),
            },
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::ToolInputJson {
                content: "{}".into(),
            },
        },
        Event::BlockStop { index: 0 },
        Event::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            stop_details: None,
            usage: UsageDelta::default(),
        },
        Event::MessageStop,
    ]
}

async fn run(
    client: Arc<Scripted>,
    compaction: Compaction,
    turn: Vec<Message>,
    seed: Seed,
) -> (Vec<Value>, RunOutcome) {
    let mut granted = grant(client);
    granted.compaction = compaction;
    let mut run = HarnessRun::start(turn, granted, seed);
    let events = support::drain_transcript(&mut run).await;
    let outcome = run.join().await.expect("the run leaves an outcome");
    (events, outcome)
}

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == kind)
        .collect()
}

#[tokio::test]
async fn a_requested_compaction_commits_the_summary_in_place_of_the_conversation() {
    let client = Arc::new(Scripted::sequence(vec![
        text_reply("SUMMARY"),
        text_reply("Got it."),
    ]));
    let (events, outcome) = run(
        client.clone(),
        compacting(None, true),
        Vec::new(),
        earlier(None),
    )
    .await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);

    // The summary call reads everything, then the instruction.
    let requests = client.requests_seen();
    assert_eq!(
        requests.len(),
        2,
        "one summary call and one acknowledgement"
    );
    let summary_request: Vec<&Message> = requests[0]
        .messages
        .iter()
        .map(|turn| match turn {
            llm::Turn::Value(message) => message,
            llm::Turn::Span(_) => panic!("the harness sends values"),
        })
        .collect();
    assert_eq!(summary_request.len(), 4);
    assert!(texts(summary_request[3]).contains("being compacted"));

    // The system prompt is kept; everything after it is the summary and the
    // model's acknowledgement.
    let committed = &outcome.committed.messages;
    assert_eq!(committed.len(), 3, "{committed:?}");
    assert_eq!(committed[0].role, Role::System);
    assert_eq!(texts(&committed[0]), "sys");
    assert_eq!(committed[1].role, Role::User);
    let summary_turn = texts(&committed[1]);
    assert!(summary_turn.contains("<summary>\nSUMMARY\n</summary>"));
    assert!(summary_turn.contains("confirming you have this summary"));
    assert_eq!(texts(&committed[2]), "Got it.");

    let compaction = of_type(&events, "compaction");
    assert_eq!(compaction.len(), 1);
    assert_eq!(compaction[0]["trigger"], "requested");
    assert_eq!(compaction[0]["summary"], "SUMMARY");
    assert_eq!(compaction[0]["summarized"], 2);
    assert_eq!(of_type(&events, "compaction_start").len(), 1);
}

#[tokio::test]
async fn a_requested_compaction_with_nothing_to_summarize_fails_the_run() {
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let seed = Seed {
        messages: vec![system("sys"), Message::user("hello?")],
        ..earlier(None)
    };
    let (_, outcome) = run(client.clone(), compacting(None, true), Vec::new(), seed).await;
    match outcome.error {
        Some(harness::RunError::Loop(Error::CompactionFailed { .. })) => {}
        other => panic!("expected a compaction failure, got {other:?}"),
    }
    assert!(
        client.requests_seen().is_empty(),
        "nothing reached the model"
    );
    assert!(outcome.committed_frame.is_none());
}

#[tokio::test]
async fn a_conversation_past_the_threshold_is_compacted_before_the_next_input() {
    let client = Arc::new(Scripted::sequence(vec![
        text_reply("SUMMARY"),
        text_reply("next answer"),
    ]));
    let (events, outcome) = run(
        client.clone(),
        compacting(Some(100), false),
        input("and now this"),
        earlier(Some(85)),
    )
    .await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);

    let committed = &outcome.committed.messages;
    let shape: Vec<(Role, String)> = committed
        .iter()
        .map(|message| (message.role, texts(message)))
        .collect();
    assert_eq!(shape.len(), 4, "{shape:?}");
    assert_eq!(shape[0], (Role::System, "sys".to_owned()));
    assert!(shape[1].1.contains("SUMMARY"));
    assert!(
        !shape[1].1.contains("confirming"),
        "input follows, so no acknowledgement is asked for"
    );
    assert_eq!(shape[2], (Role::User, "and now this".to_owned()));
    assert_eq!(shape[3], (Role::Assistant, "next answer".to_owned()));

    let compaction = of_type(&events, "compaction");
    assert_eq!(compaction[0]["trigger"], "automatic");
    assert_eq!(compaction[0]["tokensBefore"], 85);
}

#[tokio::test]
async fn a_conversation_under_the_threshold_is_left_alone() {
    let client = Arc::new(Scripted::new(text_reply("answer")));
    let (events, outcome) = run(
        client.clone(),
        compacting(Some(100), false),
        input("more"),
        earlier(Some(79)),
    )
    .await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(client.requests_seen().len(), 1);
    assert_eq!(outcome.committed.messages.len(), 5);
    assert!(of_type(&events, "compaction_start").is_empty());
}

#[tokio::test]
async fn a_step_under_way_survives_a_compaction_mid_turn() {
    let client = Arc::new(Scripted::sequence(vec![
        tool_call_measuring(90),
        text_reply("SUMMARY"),
        text_reply("done"),
    ]));
    let (events, outcome) = run(
        client.clone(),
        compacting(Some(100), false),
        input("go"),
        Seed::default(),
    )
    .await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(client.requests_seen().len(), 3);

    // The system prompt, the summary of "go", then the tool call and its
    // result exactly as they were, and the answer.
    let committed = &outcome.committed.messages;
    let roles: Vec<Role> = committed.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        vec![
            Role::System,
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant
        ]
    );
    assert!(texts(&committed[1]).contains("SUMMARY"));
    assert!(matches!(
        &committed[2].content[..],
        [ContentBlock::ToolUse { id, .. }] if id == "call_1"
    ));
    assert!(matches!(
        &committed[3].content[..],
        [ContentBlock::ToolResult { tool_use_id, .. }] if tool_use_id == "call_1"
    ));
    assert_eq!(texts(&committed[4]), "done");

    // The instruction joined the tool results' turn rather than following it.
    let summary_request = &client.requests_seen()[1];
    let llm::Turn::Value(last) = summary_request.messages.last().unwrap() else {
        panic!("the harness sends values");
    };
    assert!(matches!(
        &last.content[..],
        [ContentBlock::ToolResult { .. }, ContentBlock::Text { .. }]
    ));
    assert_eq!(of_type(&events, "compaction").len(), 1);
}

#[tokio::test]
async fn an_automatic_compaction_that_fails_is_reported_and_the_turn_goes_on() {
    let client = Arc::new(Scripted::new(text_reply("answer")));
    let seed = Seed {
        messages: vec![system("sys"), Message::user("hello?")],
        ..earlier(Some(95))
    };
    let (events, outcome) = run(
        client.clone(),
        compacting(Some(100), false),
        input("more"),
        seed,
    )
    .await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(of_type(&events, "compaction_failed").len(), 1);
    assert_eq!(
        client.requests_seen().len(),
        1,
        "the answer, and nothing else"
    );
}

#[tokio::test]
async fn the_committed_lineage_records_how_much_context_it_filled() {
    // `text_reply` reads 10 tokens and writes 4.
    let client = Arc::new(Scripted::new(text_reply("answer")));
    let (_, outcome) = run(client, Compaction::default(), input("hi"), Seed::default()).await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.committed.context_tokens, Some(14));

    let stored = serde_json::to_value(&outcome.committed).unwrap();
    let back: Seed = serde_json::from_value(stored).unwrap();
    assert_eq!(back.context_tokens, Some(14));
}

#[tokio::test]
async fn a_lineage_stored_before_sizes_were_recorded_still_seeds() {
    let mut stored = serde_json::to_value(earlier(None)).unwrap();
    assert!(
        stored.get("context_tokens").is_none(),
        "an unknown size is not written"
    );
    stored
        .as_object_mut()
        .unwrap()
        .insert("provider".into(), "scripted".into());
    let seed: Seed = serde_json::from_value(stored).unwrap();
    assert_eq!(seed.context_tokens, None);
}
