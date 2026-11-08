//! What a run writes to the console. In its own binary because the isolate
//! runs on its own thread, which only a global subscriber can observe.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use harness::{HarnessRun, Seed};
use serde_json::Value;
use support::{Scripted, grant, input, text_reply};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const SWALLOWING: &str = r#"
export default {
  execute: async function* (ctx, input) {
    ctx.log.info("from the harness\nforged line", { step: 1 });
    try {
      await ctx.functions.invoke("always.fails", null);
    } catch {
      // A harness may swallow this. The console must still show it.
    }
    yield* ctx.llm.stream({ messages: [...input] });
  }
};
"#;

#[test]
fn a_run_logs_inside_its_span_even_when_the_harness_swallows_the_failure() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .init();

    let mut grant = grant(Arc::new(Scripted::new(text_reply("hello"))));
    grant.functions.register("always.fails", |_: Value| {
        Box::pin(async { Err::<Value, _>("the database is gone".to_owned()) })
    });

    let span = tracing::info_span!("run", run_id = "run-under-test");
    let mut run = span.in_scope(|| {
        HarnessRun::start(SWALLOWING.to_string(), input("hi"), grant, Seed::default())
    });
    support::drain_transcript(&mut run);
    let outcome = support::finished(run, "the run must finish");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);

    let console = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let line = |needle: &str| {
        console
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line has `{needle}`:\n{console}"))
    };

    let harness_line = line("from the harness");
    assert!(
        harness_line.contains("run_id=\"run-under-test\""),
        "{harness_line}"
    );
    assert!(
        harness_line.contains("forged line"),
        "control characters must not split a line"
    );

    let failure = line("granted function failed");
    assert!(failure.contains("always.fails"), "{failure}");
    assert!(failure.contains("the database is gone"), "{failure}");
    assert!(failure.contains("run-under-test"), "{failure}");
}
