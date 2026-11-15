//! How fast one model call answered: the wait for its first output, and the
//! pace of the output after that.
//!
//! Measured from the arrival stamps the harness puts on a model frame's events,
//! so the figures are the provider's pacing as this process received it.
//!
//! A provider streams text, not token counts: the only token figure is the
//! total it reports when the call ends. So what is recorded over time is the
//! number of characters that arrived in each slice, which is what was observed.
//! A reader that wants tokens per second over time spreads the call's output
//! tokens across the slices in proportion to their characters.

use std::time::{Duration, Instant};

use serde::Serialize;

/// The finest slice the series is kept at.
const BASE_BUCKET_MS: u64 = 250;

/// The most slices one call keeps. A call that runs longer is kept at a coarser
/// slice rather than cut short, so the series always covers the whole call.
const MAX_BUCKETS: u64 = 2400;

/// Stored on the exchange as JSON. Every duration is in milliseconds from the
/// moment the call was dispatched.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Timing {
    /// Until the first generated content arrived: answer text, reasoning, or
    /// tool-call arguments. `None` when the call produced none.
    pub ttft_ms: Option<u64>,
    /// Until the last event of the call arrived.
    pub duration_ms: u64,
    /// Output tokens over the time between the first and the last generated
    /// content, so the wait before the first token does not drag it down.
    /// `None` when the provider reported no output tokens, or when the content
    /// arrived within one slice and there is no span to divide by.
    pub tokens_per_second: Option<f64>,
    /// The width of one slice of `chars`.
    pub bucket_ms: u64,
    /// Characters of generated content that arrived in each slice, the first
    /// slice starting at dispatch. Ends at the slice holding the last content.
    pub chars: Vec<u32>,
}

/// Folds one call's stamped events into its timing.
///
/// `output_tokens` is the provider's own total, when it reported one.
pub fn measure<'a>(
    dispatched: Instant,
    events: impl IntoIterator<Item = (Instant, &'a llm::Event)>,
    output_tokens: Option<u64>,
) -> Timing {
    let since = |at: Instant| millis(at.saturating_duration_since(dispatched));

    let mut duration_ms = 0;
    // (offset, characters) for every piece of generated content.
    let mut pieces: Vec<(u64, u32)> = Vec::new();
    for (at, event) in events {
        let offset = since(at);
        duration_ms = duration_ms.max(offset);
        let characters = generated_chars(event);
        if characters > 0 {
            pieces.push((offset, characters));
        }
    }

    let first = pieces.first().map(|(offset, _)| *offset);
    let last = pieces.last().map(|(offset, _)| *offset).unwrap_or(0);

    let mut bucket_ms = BASE_BUCKET_MS;
    while last / bucket_ms >= MAX_BUCKETS {
        bucket_ms *= 2;
    }

    let mut chars = Vec::new();
    if !pieces.is_empty() {
        chars.resize((last / bucket_ms) as usize + 1, 0u32);
        for (offset, characters) in &pieces {
            let slot = &mut chars[(offset / bucket_ms) as usize];
            *slot = slot.saturating_add(*characters);
        }
    }

    let tokens_per_second = match (output_tokens, first) {
        (Some(tokens), Some(first)) if tokens > 0 && last - first >= BASE_BUCKET_MS => {
            Some(tokens as f64 * 1000.0 / (last - first) as f64)
        }
        _ => None,
    };

    Timing {
        ttft_ms: first,
        duration_ms,
        tokens_per_second,
        bucket_ms,
        chars,
    }
}

/// How many characters of generated content an event carries.
///
/// A thinking signature is left out: it is an opaque blob that lands in one
/// piece when the block closes, and counting it would draw a burst of output
/// the model never wrote.
fn generated_chars(event: &llm::Event) -> u32 {
    let llm::Event::BlockDelta { delta, .. } = event else {
        return 0;
    };
    let content = match delta {
        llm::Delta::Text { content }
        | llm::Delta::Thinking { content }
        | llm::Delta::ToolInputJson { content } => content,
        _ => return 0,
    };
    u32::try_from(content.chars().count()).unwrap_or(u32::MAX)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(content: &str) -> llm::Event {
        llm::Event::BlockDelta {
            index: 0,
            delta: llm::Delta::Text {
                content: content.to_string(),
            },
        }
    }

    fn run(script: &[(u64, llm::Event)], output_tokens: Option<u64>) -> Timing {
        let dispatched = Instant::now();
        measure(
            dispatched,
            script
                .iter()
                .map(|(ms, event)| (dispatched + Duration::from_millis(*ms), event)),
            output_tokens,
        )
    }

    #[test]
    fn the_wait_before_content_is_the_time_to_first_token() {
        let timing = run(
            &[
                (100, llm::Event::MessageStop),
                (800, text("hello")),
                (1300, text(" there")),
                (1350, llm::Event::MessageStop),
            ],
            Some(10),
        );

        assert_eq!(timing.ttft_ms, Some(800));
        assert_eq!(timing.duration_ms, 1350);
        // 10 tokens over the 500 ms between first and last content.
        assert_eq!(timing.tokens_per_second, Some(20.0));
        assert_eq!(timing.bucket_ms, 250);
        assert_eq!(timing.chars, vec![0, 0, 0, 5, 0, 6]);
    }

    #[test]
    fn reasoning_and_tool_arguments_count_but_a_signature_does_not() {
        let delta = |delta| llm::Event::BlockDelta { index: 0, delta };
        let timing = run(
            &[
                (
                    10,
                    delta(llm::Delta::ThinkingSignature {
                        content: "opaque".into(),
                    }),
                ),
                (
                    300,
                    delta(llm::Delta::Thinking {
                        content: "hm".into(),
                    }),
                ),
                (
                    600,
                    delta(llm::Delta::ToolInputJson {
                        content: "{}".into(),
                    }),
                ),
            ],
            None,
        );

        assert_eq!(timing.ttft_ms, Some(300));
        assert_eq!(timing.chars, vec![0, 2, 2]);
        assert_eq!(timing.tokens_per_second, None);
    }

    #[test]
    fn content_that_lands_at_once_has_no_rate() {
        let timing = run(&[(900, text("all of it")), (910, text("."))], Some(400));

        assert_eq!(timing.ttft_ms, Some(900));
        assert_eq!(timing.tokens_per_second, None);
    }

    #[test]
    fn a_call_with_no_content_has_no_first_token() {
        let timing = run(&[(40, llm::Event::MessageStop)], Some(0));

        assert_eq!(timing.ttft_ms, None);
        assert_eq!(timing.duration_ms, 40);
        assert!(timing.chars.is_empty());
    }

    #[test]
    fn a_long_call_is_kept_whole_at_a_coarser_slice() {
        let twenty_minutes = 20 * 60 * 1000;
        let timing = run(&[(0, text("a")), (twenty_minutes, text("b"))], Some(2));

        assert_eq!(timing.bucket_ms, 1000);
        assert_eq!(timing.chars.len(), 1201);
        assert_eq!(timing.chars.first(), Some(&1));
        assert_eq!(timing.chars.last(), Some(&1));
    }
}
