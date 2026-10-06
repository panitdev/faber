//! The run-start lifecycle and the dispatcher, against plugins that record
//! what was called on them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plugin::bind::{self, BindError, Binding};
use plugin::run::{self, Degraded, SYSTEM_CAP, Snapshot};
use plugin::{
    ConfigError, Content, Delivery, Failure, Json, Manifest, Message, Moment, Notices, Phase,
    Plugin, Registry, SessionCtx, ToolCall, ToolResult,
};
use serde_json::json;

#[derive(Default)]
struct Calls(Mutex<Vec<String>>);

impl Calls {
    fn push(&self, call: String) {
        self.0.lock().unwrap().push(call);
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

struct Fake {
    manifest: Manifest,
    calls: Arc<Calls>,
    exports: Vec<String>,
    imports: Vec<String>,
    system: String,
    fail_open: bool,
}

fn fake(id: &str, calls: &Arc<Calls>) -> Fake {
    Fake {
        manifest: serde_json::from_value(json!({
            "id": id,
            "name": id,
            "version": "1.0.0",
            "description": "",
            "tools": [{
                "name": "echo",
                "description": "Echoes `text`.",
                "input_schema": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }
            }],
            "config": {
                "version": 2,
                "schema": { "type": "object" },
                "default": { "n": 0 }
            }
        }))
        .unwrap(),
        calls: Arc::clone(calls),
        exports: Vec::new(),
        imports: Vec::new(),
        system: format!("{id} is here."),
        fail_open: false,
    }
}

#[async_trait]
impl Plugin for Fake {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn exports(&self) -> Vec<String> {
        self.exports.clone()
    }
    fn imports(&self) -> Vec<String> {
        self.imports.clone()
    }
    async fn open(
        &self,
        ctx: &SessionCtx,
        phase: Phase,
        config: &Json,
    ) -> Result<Vec<Content>, String> {
        self.calls
            .push(format!("{} open {phase:?} {config}", self.manifest.id));
        if self.fail_open {
            return Err("no".into());
        }
        if phase == Phase::Added {
            ctx.post(
                Message::user(format!("{} joined", self.manifest.id)),
                Delivery::OnMessage,
            )
            .unwrap();
        }
        Ok(vec![Content::text(self.system.clone())])
    }
    async fn handle_tool(&self, _ctx: &SessionCtx, config: &Json, call: ToolCall) -> ToolResult {
        self.calls
            .push(format!("{} {} {config}", self.manifest.id, call.name));
        if call.input["text"] == "panic" {
            panic!("asked to");
        }
        ToolResult::ok(call.input["text"].as_str().unwrap_or_default())
    }
    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        if config
            .get("n")
            .and_then(Json::as_i64)
            .is_some_and(|n| n < 0)
        {
            return Err(vec![ConfigError::new("/n", "must not be negative")]);
        }
        Ok(config)
    }
    fn migrate(&self, config: Json, from: u32) -> Result<Json, Vec<ConfigError>> {
        assert_eq!(from, 1);
        Ok(json!({ "n": config["count"] }))
    }
    fn on_changed(&self, old: &Json, new: &Json) -> Vec<Message> {
        self.calls
            .push(format!("{} changed {old} -> {new}", self.manifest.id));
        vec![Message::user(format!(
            "{} config changed",
            self.manifest.id
        ))]
    }
    fn on_removed(&self, config: &Json) -> Vec<Message> {
        self.calls
            .push(format!("{} removed {config}", self.manifest.id));
        vec![Message::user(format!("{} is gone", self.manifest.id))]
    }
}

fn binding(plugin: &str, order: i64, config: Json) -> Binding {
    Binding {
        plugin: plugin.into(),
        version: "1.0.0".into(),
        config_version: 2,
        config,
        enabled: true,
        order,
    }
}

fn registry(plugins: Vec<Fake>) -> Registry {
    let mut registry = Registry::new();
    for plugin in plugins {
        registry.register(Arc::new(plugin), true).unwrap();
    }
    registry
}

#[tokio::test]
async fn a_first_run_opens_fresh_and_composes_the_head_in_creation_order() {
    let calls = Arc::new(Calls::default());
    let registry = registry(vec![fake("alpha", &calls), fake("beta", &calls)]);
    let notices = Notices::new();
    let started = run::start(
        &registry,
        "s1",
        &[
            binding("beta", 2, json!({})),
            binding("alpha", 1, json!({})),
        ],
        None,
        &notices,
        "Core.",
    )
    .await;

    assert_eq!(
        started.head.as_deref(),
        Some("Core.\n\nalpha is here.\n\nbeta is here.")
    );
    let names: Vec<_> = started
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(names, ["alpha__echo", "beta__echo"]);
    assert_eq!(calls.take(), ["alpha open Fresh {}", "beta open Fresh {}"]);
}

#[tokio::test]
async fn later_runs_open_only_what_changed_and_hooks_land_before_open_notices() {
    let calls = Arc::new(Calls::default());
    let registry = registry(vec![
        fake("alpha", &calls),
        fake("beta", &calls),
        fake("gamma", &calls),
    ]);
    let notices = Notices::new();
    let first = run::start(
        &registry,
        "s1",
        &[
            binding("alpha", 1, json!({ "n": 1 })),
            binding("beta", 2, json!({})),
        ],
        None,
        &notices,
        "Core.",
    )
    .await;
    calls.take();

    // alpha changed, beta removed, gamma added.
    let second = run::start(
        &registry,
        "s1",
        &[
            binding("alpha", 1, json!({ "n": 2 })),
            binding("gamma", 3, json!({})),
        ],
        Some(&first.snapshot),
        &notices,
        "Core.",
    )
    .await;

    assert!(second.head.is_none());
    assert_eq!(
        calls.take(),
        [
            "beta removed {}",
            "alpha changed {\"n\":1} -> {\"n\":2}",
            "alpha open Reopened {\"n\":2}",
            "gamma open Added {}",
        ]
    );
    assert_eq!(
        notices.drain(Moment::Message),
        vec![Message::user(
            "alpha config changed\n\nbeta is gone\n\ngamma joined"
        )]
    );

    // Nothing changed: nothing is opened, nothing is said.
    let third = run::start(
        &registry,
        "s1",
        &[
            binding("alpha", 1, json!({ "n": 2 })),
            binding("gamma", 3, json!({})),
        ],
        Some(&second.snapshot),
        &notices,
        "Core.",
    )
    .await;
    assert!(calls.take().is_empty());
    assert_eq!(third.snapshot, second.snapshot);
}

#[tokio::test]
async fn changes_while_idle_collapse_into_one() {
    let calls = Arc::new(Calls::default());
    let registry = registry(vec![fake("alpha", &calls)]);
    let notices = Notices::new();
    let first = run::start(
        &registry,
        "s",
        &[binding("alpha", 1, json!({ "n": 1 }))],
        None,
        &notices,
        "",
    )
    .await;
    calls.take();
    // n: 1 -> 2 -> 3 while idle is one change, 1 -> 3.
    run::start(
        &registry,
        "s",
        &[binding("alpha", 1, json!({ "n": 3 }))],
        Some(&first.snapshot),
        &notices,
        "",
    )
    .await;
    assert_eq!(calls.take()[0], "alpha changed {\"n\":1} -> {\"n\":3}");
}

#[tokio::test]
async fn a_failed_open_degrades_and_the_core_answers_its_tools() {
    let calls = Arc::new(Calls::default());
    let mut broken = fake("broken", &calls);
    broken.fail_open = true;
    let mut big = fake("big", &calls);
    big.system = "x".repeat(SYSTEM_CAP + 1);
    let registry = registry(vec![broken, big]);
    let notices = Notices::new();
    let started = run::start(
        &registry,
        "s",
        &[
            binding("broken", 1, json!({})),
            binding("big", 2, json!({})),
        ],
        None,
        &notices,
        "Core.",
    )
    .await;

    assert_eq!(started.head.as_deref(), Some("Core."));
    let degraded: Vec<_> = started.report.iter().map(|r| r.degraded).collect();
    assert_eq!(
        degraded,
        [Some(Degraded::OpenFailed), Some(Degraded::SystemTooLarge)]
    );
    // Its tools stay in the array.
    assert_eq!(started.tools.len(), 2);

    let result = started
        .dispatcher
        .invoke("broken__echo", json!({ "text": "hi" }), None)
        .await;
    assert_eq!(result.error, Some(Failure::HandlerFailure));
    assert!(!plugin::types::joined(&result.content).contains("open-failed"));

    // The next run start retries open.
    calls.take();
    run::start(
        &registry,
        "s",
        &[
            binding("broken", 1, json!({})),
            binding("big", 2, json!({})),
        ],
        Some(&started.snapshot),
        &notices,
        "",
    )
    .await;
    assert_eq!(
        calls.take(),
        ["broken open Reopened {}", "big open Reopened {}"]
    );
}

#[tokio::test]
async fn a_missing_provider_degrades_its_importer() {
    let calls = Arc::new(Calls::default());
    let mut provider = fake("provider", &calls);
    provider.exports = vec!["faber:test/handle".into()];
    let mut importer = fake("importer", &calls);
    importer.imports = vec!["faber:test/handle".into()];
    let registry = registry(vec![provider, importer]);
    let notices = Notices::new();

    // The importer was created first, but its provider opens first.
    let both = [
        binding("importer", 1, json!({})),
        binding("provider", 2, json!({})),
    ];
    run::start(&registry, "s", &both, None, &notices, "").await;
    assert_eq!(
        calls.take(),
        ["provider open Fresh {}", "importer open Fresh {}"]
    );

    let mut disabled = both.clone();
    disabled[1].enabled = false;
    let started = run::start(&registry, "s", &disabled, None, &notices, "").await;
    assert_eq!(
        started.report[0].degraded,
        Some(Degraded::DependencyMissing)
    );
    let result = started
        .dispatcher
        .invoke("importer__echo", json!({ "text": "x" }), None)
        .await;
    assert_eq!(result.error, Some(Failure::UpstreamFailure));

    assert_eq!(bind::importers(&registry, &both, "provider"), ["importer"]);
}

#[tokio::test]
async fn the_dispatcher_writes_bad_requests_and_survives_a_panic() {
    let calls = Arc::new(Calls::default());
    let registry = registry(vec![fake("alpha", &calls)]);
    let notices = Notices::new();
    let started = run::start(
        &registry,
        "s",
        &[binding("alpha", 1, json!({ "n": 5 }))],
        None,
        &notices,
        "",
    )
    .await;
    let dispatcher = started.dispatcher;

    let ok = dispatcher
        .invoke("alpha__echo", json!({ "text": "hi" }), None)
        .await;
    assert_eq!(ok, ToolResult::ok("hi"));
    assert!(calls.take().contains(&"alpha echo {\"n\":5}".to_owned()));

    let unknown = dispatcher.invoke("echo", json!({}), None).await;
    assert_eq!(unknown.error, Some(Failure::BadRequest));

    let schema = dispatcher
        .invoke("alpha__echo", json!({ "text": 1 }), None)
        .await;
    assert_eq!(schema.error, Some(Failure::BadRequest));
    assert!(plugin::types::joined(&schema.content).contains("/text: expected string"));
    assert!(
        calls.take().is_empty(),
        "a schema failure never reaches the plugin"
    );

    let panicked = dispatcher
        .invoke("alpha__echo", json!({ "text": "panic" }), None)
        .await;
    assert_eq!(panicked.error, Some(Failure::HandlerFailure));
}

#[tokio::test]
async fn bind_time_migrates_validates_and_refuses_cycles() {
    let calls = Arc::new(Calls::default());
    let mut a = fake("a", &calls);
    a.exports = vec!["x:a/i".into()];
    a.imports = vec!["x:b/i".into()];
    let mut b = fake("b", &calls);
    b.exports = vec!["x:b/i".into()];
    b.imports = vec!["x:a/i".into()];
    let registry = registry(vec![a, b]);

    let prepared = bind::prepare(&registry, "a", None, json!({ "count": 4 }), Some(1)).unwrap();
    assert_eq!(prepared.config, json!({ "n": 4 }));
    assert_eq!(prepared.config_version, 2);

    match bind::prepare(&registry, "a", None, json!({ "n": -1 }), None) {
        Err(BindError::ConfigInvalid(errors)) => assert_eq!(errors[0].path, "/n"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        bind::prepare(&registry, "nope", None, json!({}), None),
        Err(BindError::UnknownType(_))
    ));

    let cycle = bind::check_graph(
        &registry,
        &[binding("a", 1, json!({})), binding("b", 2, json!({}))],
    );
    assert!(matches!(cycle, Err(BindError::DependencyCycle(_))));
}

#[test]
fn a_snapshot_round_trips_through_json() {
    let snapshot = Snapshot {
        bindings: vec![run::SnapshotEntry {
            plugin: "alpha".into(),
            version: "1.0.0".into(),
            config: json!({}),
            order: 1,
            degraded: Some(Degraded::DependencyMissing),
        }],
    };
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["bindings"][0]["degraded"], "dependency-missing");
    assert_eq!(serde_json::from_value::<Snapshot>(value).unwrap(), snapshot);
}
