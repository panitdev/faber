//! Runs a harness file against a real Anthropic model and prints the
//! transcript as it streams, then the frame log.
//!
//! ```text
//! ANTHROPIC_API_KEY=... cargo run -p harness --example run -- \
//!   harnesses/identity.js "hello"
//! ```

use std::sync::Arc;

use harness::{Grant, HarnessRun, Seed};
use llm::{ContentBlock, Message, Role};
use secrecy::SecretString;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let harness_path = args.next().expect("usage: run <harness.js> <prompt>");
    let prompt = args.next().unwrap_or_else(|| "hello".to_string());

    let harness_source =
        std::fs::read_to_string(&harness_path).expect("failed to read harness file");

    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
    let client =
        llm::anthropic::Anthropic::new(llm::anthropic::Config::new(SecretString::from(api_key)))
            .expect("failed to build the Anthropic client");

    let grant = Grant {
        client: Arc::new(client),
        model: llm::anthropic::DEFAULT_MODEL.to_string(),
        reasoning_history: None,
        // No selection to resolve here: whatever the model reasons by default
        // is what this example asks for.
        reasoning: None,
        advanced_options: llm::AdvancedOptions::default(),
        tools: Vec::new(),
        tool_invoker: None,
        commit_granted: true,
        functions: harness::FunctionRegistry::new(),
        // Nothing here can ask this run to stop — the example runs it to
        // completion or not at all.
        interrupt: None,
    };

    let input = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: prompt }],
    }];

    // No prior conversation to seed from — this is a one-shot CLI, not a
    // multi-turn session.
    let mut run = HarnessRun::start(harness_source, input, grant, Seed::default());

    println!("--- transcript ---");
    while let Some(event) = run.transcript.recv().await {
        match event.get("type").and_then(|t| t.as_str()) {
            Some("block_delta") => {
                if let Some(text) = event["delta"]["text"].as_str() {
                    print!("{text}");
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
            }
            Some(other) => {
                println!("\n[{other}]");
            }
            None => {}
        }
    }
    println!("\n--- end transcript ---");

    match tokio::task::spawn_blocking(move || run.join()).await {
        Ok(Ok(outcome)) => {
            println!("--- frame log ({} events) ---", outcome.frames.len());
            for frame in outcome.frames {
                println!("{frame:?}");
            }
            println!(
                "--- committed lineage: {} messages (frame {:?}) ---",
                outcome.committed.messages.len(),
                outcome.committed_frame
            );
        }
        Ok(Err(error)) => eprintln!("run failed: {error}"),
        Err(join_error) => eprintln!("harness thread panicked: {join_error}"),
    }
}
