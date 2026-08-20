//! The web surface, granted beside the environment one.
//!
//! The claim under test is that two projections reach the loop as a single
//! grant: the model calls a tool from each, both are dispatched, and neither
//! surface has to know the other exists. The engine is canned — what is real
//! here is the harness source, the toolbox, and the round trip.

mod support;

use std::sync::Arc;

use environment::{Blobs, LocalTarget, MemoryBlobs, Registry, Root};
use harness::{HarnessRun, Seed, Surface, Toolbox, Web};
use search::{Hit, Query, Results, SearchEngine};
use support::{Scripted, drain_transcript, grant, input, text_reply, tool_call_reply};

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("faber-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp directory");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One instance that always answers with the same hit.
struct Canned;

#[async_trait::async_trait]
impl SearchEngine for Canned {
    async fn search(&self, query: &Query) -> search::Result<Results> {
        Ok(Results {
            query: query.text.clone(),
            hits: vec![Hit {
                url: "https://doc.rust-lang.org/book/ch04-01".to_owned(),
                title: "What is Ownership?".to_owned(),
                snippet: "Ownership is a set of rules".to_owned(),
                engines: vec!["duckduckgo".to_owned()],
                score: 1.0,
                category: None,
                published: None,
                thumbnail: None,
            }],
            source: Some("searx.example.org".to_owned()),
            ..Results::default()
        })
    }

    fn provider(&self) -> &str {
        "canned"
    }
}

/// Both projections, granted as one — the shape `crates/api` builds per run.
fn toolbox_over(root: &std::path::Path) -> Toolbox {
    let blobs: Arc<dyn Blobs> = Arc::new(MemoryBlobs::new());
    let machine = tokio::runtime::Runtime::new()
        .expect("a runtime")
        .block_on(LocalTarget::bind(
            "build",
            Root::new(root.to_str().expect("a utf-8 temp path")).expect("a root"),
            Arc::clone(&blobs),
        ))
        .expect("binding a local target");

    let mut registry = Registry::new();
    registry
        .bind("build", Arc::new(machine))
        .expect("a fresh label");
    let surface = Arc::new(Surface::new(Arc::new(registry), blobs));
    let web = Arc::new(Web::new(Arc::new(Canned)));

    let mut toolbox = Toolbox::new();
    toolbox.add(Surface::definitions(), surface.invoker());
    toolbox.add(Web::definitions(), web.invoker());
    toolbox
}

#[test]
fn the_shipped_harness_searches_the_web_and_writes_what_it_found() {
    let dir = TempDir::new("granted");

    let client = Arc::new(Scripted::sequence(vec![
        tool_call_reply("call_1", "search", r#"{"query":"rust ownership"}"#),
        // The second call proves the composition rather than a swap: the
        // environment surface is still reachable from the same grant.
        tool_call_reply(
            "call_2",
            "patch",
            &format!(
                r#"{{"execute_in":"build","ops":[{{"op":"add","path":"{}","content":"the book\n"}}]}}"#,
                dir.0.join("found.txt").display()
            ),
        ),
        text_reply("the book explains ownership"),
    ]));

    let mut granted = grant(client);
    let toolbox = toolbox_over(&dir.0);
    granted.tools = toolbox.definitions();
    granted.tool_invoker = Some(toolbox.invoker());

    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_owned(),
        input("where is rust ownership documented?"),
        granted,
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let results: Vec<&serde_json::Value> = events
        .iter()
        .filter(|event| event["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 2, "both calls were dispatched");

    let searched = results[0]["content"].as_str().expect("a rendered result");
    assert_eq!(results[0]["isError"], false);
    // The instance that served it, and the link itself.
    assert!(searched.contains("searx.example.org"), "{searched}");
    assert!(searched.contains("https://doc.rust-lang.org/book/ch04-01"));

    assert_eq!(results[1]["isError"], false);
    assert!(dir.0.join("found.txt").exists(), "the patch also ran");
}
