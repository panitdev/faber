//! What a run writes to the console. In its own binary because a run is a
//! task of its own, which only a global subscriber can observe.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use harness::mapping::LLMRequest;
use harness::{Context, Error, HarnessRun, Seed};
use serde_json::Value;
use support::{Scripted, Wire, forward, grant, input, text_reply};

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

/// Logs a line, ignores a failing function, then answers.
async fn swallowing(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
    tracing::info!("from the loop");
    // A loop may carry on past this. The console must still show it.
    let _ = ctx.invoke_function("always.fails", Value::Null).await;
    let call = ctx.stream(LLMRequest::new(input))?;
    forward(&ctx, &call).await
}

#[tokio::test]
async fn a_run_logs_inside_its_span_even_when_the_loop_ignores_the_failure() {
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
    let mut run =
        span.in_scope(|| HarnessRun::start_with(swallowing, input("hi"), grant, Seed::default()));
    support::drain_transcript(&mut run).await;
    support::finished(run, "the run must finish").await;

    let console = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let line = |needle: &str| {
        console
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line has `{needle}`:\n{console}"))
    };

    let loop_line = line("from the loop");
    assert!(
        loop_line.contains("run_id=\"run-under-test\""),
        "{loop_line}"
    );

    let failure = line("granted function failed");
    assert!(failure.contains("always.fails"), "{failure}");
    assert!(failure.contains("the database is gone"), "{failure}");
    assert!(failure.contains("run-under-test"), "{failure}");
}
