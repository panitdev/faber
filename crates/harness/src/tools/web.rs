//! Web search, projected as a tool.
//!
//! A second projection beside [`Surface`](super::Surface), and deliberately not
//! part of it: search runs against no target, has no root, and answers nothing
//! the environment contract describes. Sharing a module would mean a second
//! carve-out in a surface whose whole shape is "every call names the machine it
//! runs on".
//!
//! What this projection has to preserve, because the crate below it went to the
//! trouble of keeping the distinctions apart:
//!
//! - **A query that matched nothing is a result.** It comes back with
//!   `is_error` false and says the search ran, because "no such thing exists on
//!   the web" is an answer a model will act on and must not be manufactured
//!   from a backend that never got asked.
//! - **A backend that could not be asked is a fault**, and its message says the
//!   query itself is untouched — the same repair instruction an unreachable
//!   environment carries, for the same reason.
//! - **Engines that failed are named.** Thin results with half the upstreams
//!   unresponsive is a degraded instance, not a thin web, and a renderer that
//!   drops that teaches the model the wrong lesson about the world.
//! - **The instance that served is echoed**, so a transcript says where an
//!   answer came from rather than implying the web spoke with one voice.

use std::sync::Arc;

use futures_util::FutureExt;
use search::{Error, Query, Results, SearchEngine, TimeRange};
use serde_json::{Value, json};

use crate::mapping::ToolResult;
use crate::state::ToolInvoker;

use super::{optional_string, optional_u64, string};

/// The most hits one call will return. Not a policy about the web — a bound on
/// how much of a context window one call may spend.
const MAX_LIMIT: u64 = 50;

/// The default, when the model does not say. Roughly one instance page.
const DEFAULT_LIMIT: u64 = 10;

/// Web search, bound to one engine.
///
/// The engine is whatever the caller granted: a single instance it named, or a
/// pool spreading queries across the public network. Which one is not visible
/// here and must not become visible in the schema — the model asks the web a
/// question, and the deployment decides who answers it.
pub struct Web {
    engine: Arc<dyn SearchEngine>,
}

impl Web {
    pub fn new(engine: Arc<dyn SearchEngine>) -> Self {
        Web { engine }
    }

    /// The tool definitions, granted only when an engine exists.
    ///
    /// There is no "search is not configured" result, and there should not be:
    /// a deployment without an engine simply never adds these to the grant, and
    /// the loop has one move fewer. Unlike the binding set, this cannot change
    /// mid-run, so nothing about the prefix argues for a tool that is always
    /// present and always refuses.
    pub fn definitions() -> Vec<llm::ToolDef> {
        vec![search_tool()]
    }

    /// The dispatcher, in the shape [`Grant`](crate::Grant) takes.
    pub fn invoker(self: Arc<Self>) -> ToolInvoker {
        Arc::new(move |name, input| {
            let web = Arc::clone(&self);
            async move { Ok(web.invoke(&name, input).await) }.boxed()
        })
    }

    async fn invoke(&self, name: &str, input: Value) -> ToolResult {
        match self.route(name, input).await {
            Ok(content) => ToolResult {
                content,
                is_error: false,
            },
            Err(content) => ToolResult {
                content,
                is_error: true,
            },
        }
    }

    async fn route(&self, name: &str, input: Value) -> Result<String, String> {
        match name {
            "search" => self.search(&input).await,
            other => Err(format!("`{other}` is not one of this surface's tools")),
        }
    }

    async fn search(&self, input: &Value) -> Result<String, String> {
        let query = query(input)?;
        match self.engine.search(&query).await {
            Ok(results) => Ok(render(&query, &results)),
            Err(error) => Err(fault(self.engine.provider(), &error)),
        }
    }
}

/// Reads a call into a [`Query`].
fn query(input: &Value) -> Result<Query, String> {
    let text = string(input, "query")?;
    if text.trim().is_empty() {
        return Err("`query` is empty; there is nothing to search for".to_owned());
    }

    let limit = optional_u64(input, "limit")?.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(format!(
            "`limit` must be between 1 and {MAX_LIMIT}; got {limit}"
        ));
    }

    let mut query = Query::new(text).with_limit(limit as usize);

    if let Some(range) = optional_string(input, "time_range")? {
        query = query.with_time_range(match range.as_str() {
            "day" => TimeRange::Day,
            "week" => TimeRange::Week,
            "month" => TimeRange::Month,
            "year" => TimeRange::Year,
            other => {
                return Err(format!(
                    "`{other}` is not a time range; use day, week, month, or year"
                ));
            }
        });
    }

    if let Some(category) = optional_string(input, "category")? {
        query = query.with_categories([category]);
    }

    Ok(query)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// What the web said.
fn render(query: &Query, results: &Results) -> String {
    use std::fmt::Write as _;

    let served_by = results.source.as_deref().unwrap_or("an unnamed instance");
    let mut out = String::new();

    // The query as the instance ran it, not as it was typed: instances
    // normalise, and a model comparing two searches needs the version that
    // actually produced these hits.
    let ran = if results.query.is_empty() {
        query.text.clone()
    } else {
        results.query.clone()
    };
    // A full page is a capped page as far as anyone here can tell — the limit
    // is applied to the response, and nothing in it says how much was cut. Said
    // out loud for the same reason a capped capture says TRUNCATED: a model
    // that reads ten hits as the whole answer stops looking on the strength of
    // a number it chose itself.
    let capped = query.limit.is_some_and(|limit| results.hits.len() >= limit);
    let _ = writeln!(
        out,
        "{served_by} — {} hits for \"{ran}\"{}",
        results.hits.len(),
        if capped {
            " — as many as were asked for, so there are probably more; raise `limit` for them"
        } else {
            ""
        }
    );

    for answer in &results.answers {
        out.push('\n');
        let _ = write!(out, "answer: {}", answer.text);
        if let Some(url) = &answer.url {
            let _ = write!(out, " ({url})");
        }
        out.push('\n');
    }

    if results.hits.is_empty() {
        // A settled negative answer, and said as one. The web having nothing
        // and a backend having said nothing are different facts, and only this
        // one licenses the model to stop looking.
        out.push_str(
            "\nThe search ran and matched nothing. This is an answer about the query, \
             not a failure — a different wording is more likely to help than the same \
             one again.\n",
        );
    }

    for (index, hit) in results.hits.iter().enumerate() {
        let _ = writeln!(out, "\n{}. {}", index + 1, hit.title);
        let _ = writeln!(out, "   {}", hit.url);
        if !hit.snippet.is_empty() {
            let _ = writeln!(out, "   {}", hit.snippet);
        }
        let mut notes = Vec::new();
        if !hit.engines.is_empty() {
            notes.push(hit.engines.join(", "));
        }
        if let Some(published) = &hit.published {
            notes.push(published.clone());
        }
        if !notes.is_empty() {
            let _ = writeln!(out, "   [{}]", notes.join(" · "));
        }
    }

    if !results.corrections.is_empty() {
        let _ = writeln!(out, "\ndid you mean: {}", results.corrections.join(" · "));
    }
    if !results.suggestions.is_empty() {
        let _ = writeln!(
            out,
            "\nrelated searches: {}",
            results.suggestions.join(" · ")
        );
    }

    // Never dropped, even beside a full page of hits: it is the difference
    // between "the web is thin here" and "half of this instance was down".
    if !results.unresponsive_engines.is_empty() {
        let _ = writeln!(
            out,
            "\nThese upstream engines did not answer for this query: {}. \
             Results are thinner than this instance can normally serve, so a thin \
             answer here is not evidence the web is thin.",
            results.unresponsive_engines.join(", ")
        );
    }

    out
}

/// A search that could not be run.
///
/// Split the way a caller has to act on it: a backend that was busy, rate
/// limited, or unreachable says nothing whatsoever about the query, and asking
/// again later is reasonable. A misconfigured or unusable backend is not
/// something the model can word its way around, and it is told so rather than
/// left to rephrase into the same wall.
fn fault(provider: &str, error: &Error) -> String {
    if error.is_transient() {
        format!(
            "Web search is temporarily unavailable ({provider}): {error}\n\
             This does not mean the web has no answer, and rewording the query will not \
             help. Try again later, or continue with what you already know and say that \
             you could not search."
        )
    } else {
        format!(
            "Web search failed ({provider}): {error}\n\
             Your query did not cause this, and rewording it will not help. Do not search \
             again; tell the user web search is failing."
        )
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// One tool, and the reasons its parameter list is this short.
///
/// `engines` is absent because the model cannot know which upstreams an
/// instance enabled, and naming one it has not produces a quietly thinner
/// answer rather than an error. `safe_search` is absent because it is the
/// deployment's policy and not a per-call decision. `page` is absent because
/// `limit` covers wanting more and deep paging is how a public instance
/// decides it has had enough of us.
///
/// The first clause says *the web* deliberately: this surface has no `grep`
/// and no `find`, so a tool merely named `search` is one a model will reach
/// for when it wants to search code.
fn search_tool() -> llm::ToolDef {
    llm::ToolDef {
        name: "search".to_owned(),
        description: "Search the public web and get back ranked links with snippets. \
                      Not a code or file search — use the shell for those. Snippets are \
                      the search engine's, not the page: open a link to read it."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search terms, as a person would type them.",
                },
                "limit": {
                    "type": "integer",
                    "description": "How many hits to return. Defaults to 10.",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                },
                "time_range": {
                    "type": "string",
                    "description": "Only results this recent.",
                    "enum": ["day", "week", "month", "year"],
                },
                "category": {
                    "type": "string",
                    "description": "Which index to search. Defaults to the general web.",
                    "enum": ["general", "news", "science", "it", "images", "videos"],
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use search::{Answer, Hit};

    use super::*;

    /// An engine that answers with whatever the test put in it.
    struct Canned {
        answer: Mutex<Option<search::Result<Results>>>,
        seen: Mutex<Option<Query>>,
    }

    impl Canned {
        fn new(answer: search::Result<Results>) -> Arc<Self> {
            Arc::new(Canned {
                answer: Mutex::new(Some(answer)),
                seen: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl SearchEngine for Canned {
        async fn search(&self, query: &Query) -> search::Result<Results> {
            *self.seen.lock().expect("a live lock") = Some(query.clone());
            self.answer
                .lock()
                .expect("a live lock")
                .take()
                .expect("one call per canned answer")
        }

        fn provider(&self) -> &str {
            "canned"
        }
    }

    fn hit(title: &str, url: &str) -> Hit {
        Hit {
            url: url.to_owned(),
            title: title.to_owned(),
            snippet: "a snippet".to_owned(),
            engines: vec!["google".to_owned()],
            score: 1.0,
            category: None,
            published: None,
            thumbnail: None,
        }
    }

    fn web(answer: search::Result<Results>) -> (Web, Arc<Canned>) {
        let engine = Canned::new(answer);
        (
            Web::new(Arc::clone(&engine) as Arc<dyn SearchEngine>),
            engine,
        )
    }

    #[tokio::test]
    async fn hits_come_back_with_the_instance_that_served_them() {
        let (web, _) = web(Ok(Results {
            query: "rust ownership".to_owned(),
            hits: vec![hit("Ownership", "https://doc.rust-lang.org/book/ch04")],
            source: Some("searx.example.org".to_owned()),
            ..Results::default()
        }));

        let result = web
            .invoke("search", json!({ "query": "rust ownership" }))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("searx.example.org"));
        assert!(
            result
                .content
                .contains("https://doc.rust-lang.org/book/ch04")
        );
        assert!(result.content.contains("a snippet"));
    }

    /// The distinction the crate below is built around, and the one that is
    /// silently lost if this projection is careless: nothing found is an
    /// answer, and it is never rendered as an absence.
    #[tokio::test]
    async fn nothing_found_is_an_answer_and_says_the_search_ran() {
        let (web, _) = web(Ok(Results {
            query: "qzzx no such thing".to_owned(),
            source: Some("searx.example.org".to_owned()),
            suggestions: vec!["quartz".to_owned()],
            ..Results::default()
        }));

        let result = web
            .invoke("search", json!({ "query": "qzzx no such thing" }))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("matched nothing"));
        assert!(result.content.contains("not a failure"));
        assert!(result.content.contains("quartz"));
    }

    /// The other half of the same distinction: a backend that was never asked
    /// must not read as a web that has nothing.
    #[tokio::test]
    async fn a_backend_that_could_not_answer_is_a_fault_that_clears_the_query() {
        let (web, _) = web(Err(Error::NoInstance {
            considered: 40,
            verified: 0,
        }));

        let result = web.invoke("search", json!({ "query": "anything" })).await;
        assert!(result.is_error);
        assert!(result.content.contains("unavailable"));
        assert!(result.content.contains("does not mean the web has no answer"));
        assert!(!result.content.contains("matched nothing"));
    }

    #[tokio::test]
    async fn a_rate_limit_reads_as_transient_and_a_misconfiguration_does_not() {
        let (limiting, _) = web(Err(Error::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        }));
        let limited = limiting
            .invoke("search", json!({ "query": "anything" }))
            .await;
        assert!(limited.content.contains("Try again later"));

        let (misconfigured, _) = web(Err(Error::Config("not a url".to_owned())));
        let broken = misconfigured
            .invoke("search", json!({ "query": "anything" }))
            .await;
        assert!(broken.content.contains("rewording it will not help"));
    }

    /// A page as long as the limit says so. The engine truncates to the limit
    /// and the response carries no flag for it, so a full page read as the
    /// whole of the web is the one wrong conclusion available here.
    #[tokio::test]
    async fn a_page_filled_to_the_limit_says_there_is_probably_more() {
        let (filled, _) = web(Ok(Results {
            query: "rust".to_owned(),
            hits: vec![
                hit("One", "https://one.example"),
                hit("Two", "https://two.example"),
            ],
            source: Some("searx.example.org".to_owned()),
            ..Results::default()
        }));
        let full = filled
            .invoke("search", json!({ "query": "rust", "limit": 2 }))
            .await;
        assert!(full.content.contains("probably more"), "{}", full.content);

        let (partial, _) = web(Ok(Results {
            query: "rust".to_owned(),
            hits: vec![hit("One", "https://one.example")],
            source: Some("searx.example.org".to_owned()),
            ..Results::default()
        }));
        let short = partial
            .invoke("search", json!({ "query": "rust", "limit": 2 }))
            .await;
        assert!(
            !short.content.contains("probably more"),
            "{}",
            short.content
        );
    }

    /// Thin results plus dead upstreams is a fact about the instance, and the
    /// model cannot notice it if the renderer keeps it.
    #[tokio::test]
    async fn unresponsive_engines_are_named_beside_the_results() {
        let (web, _) = web(Ok(Results {
            query: "rust".to_owned(),
            hits: vec![hit("One", "https://example.org")],
            unresponsive_engines: vec!["google".to_owned(), "brave".to_owned()],
            source: Some("searx.example.org".to_owned()),
            ..Results::default()
        }));

        let result = web.invoke("search", json!({ "query": "rust" })).await;
        assert!(!result.is_error);
        assert!(result.content.contains("google, brave"));
        assert!(result.content.contains("not evidence the web is thin"));
    }

    #[tokio::test]
    async fn a_direct_answer_is_carried_through() {
        let (web, _) = web(Ok(Results {
            query: "12 in binary".to_owned(),
            answers: vec![Answer {
                text: "1100".to_owned(),
                url: None,
            }],
            source: Some("searx.example.org".to_owned()),
            ..Results::default()
        }));

        let result = web
            .invoke("search", json!({ "query": "12 in binary" }))
            .await;
        assert!(result.content.contains("answer: 1100"));
    }

    #[tokio::test]
    async fn the_narrowing_parameters_reach_the_query() {
        let (web, engine) = web(Ok(Results::default()));
        let result = web
            .invoke(
                "search",
                json!({
                    "query": "rust release",
                    "limit": 3,
                    "time_range": "week",
                    "category": "news",
                }),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);

        let seen = engine.seen.lock().expect("a live lock").clone();
        let seen = seen.expect("the engine was called");
        assert_eq!(seen.limit, Some(3));
        assert_eq!(seen.time_range, Some(TimeRange::Week));
        assert_eq!(seen.categories, vec!["news".to_owned()]);
    }

    #[tokio::test]
    async fn a_malformed_call_names_the_parameter_rather_than_searching() {
        let (web, engine) = web(Ok(Results::default()));

        let missing = web.invoke("search", json!({})).await;
        assert!(missing.is_error);
        assert!(missing.content.contains("`query` is required"));

        let empty = web.invoke("search", json!({ "query": "   " })).await;
        assert!(empty.is_error);
        assert!(empty.content.contains("nothing to search for"));

        let overlarge = web
            .invoke("search", json!({ "query": "rust", "limit": 500 }))
            .await;
        assert!(overlarge.is_error);
        assert!(overlarge.content.contains("`limit` must be between"));

        let range = web
            .invoke(
                "search",
                json!({ "query": "rust", "time_range": "fortnight" }),
            )
            .await;
        assert!(range.is_error);
        assert!(range.content.contains("is not a time range"));

        // None of that reached the engine: a malformed call is not a search.
        assert!(engine.seen.lock().expect("a live lock").is_none());
    }

    /// This surface names no target and must not grow one: a search runs
    /// against the web, and routing it through an environment would make the
    /// answer depend on which machine was named.
    #[test]
    fn search_takes_no_target() {
        let tool = &Web::definitions()[0];
        assert_eq!(tool.name, "search");
        assert!(tool.input_schema["properties"].get("execute_in").is_none());
        assert!(tool.description.len() < 400, "{}", tool.description.len());
    }
}
