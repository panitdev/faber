//! The web-platform extensions are wired and reachable from a harness.
//!
//! They register ops and lazy scripts but install no globals, so a harness
//! reaches them through `Deno.core.loadExtScript("ext:...")`. `fetch` also
//! proves the permissions container is present and denying by default.

mod support;

use std::sync::Arc;

use harness::{HarnessRun, Seed};
use support::{Scripted, drain_transcript, grant, text_reply};

const PROBE: &str = r#"
export default {
  async *execute() {
    const url = Deno.core.loadExtScript("ext:deno_web/00_url.js");
    const encoding = Deno.core.loadExtScript("ext:deno_web/08_text_encoding.js");
    const base64 = Deno.core.loadExtScript("ext:deno_web/05_base64.js");
    const cryptoMod = Deno.core.loadExtScript("ext:deno_crypto/00_crypto.js");
    // Core installs no globals; the fetch polyfill still reaches for `URL`
    // internally, so a caller that wants fetch has to wire that one itself.
    globalThis.URL = url.URL;
    const fetchMod = Deno.core.loadExtScript("ext:deno_fetch/26_fetch.js");

    const parsed = new url.URL("https://example.com/a?b=c");
    yield { type: "probe", value: parsed.searchParams.get("b") };

    const bytes = new encoding.TextEncoder().encode("hi");
    yield { type: "probe", value: bytes instanceof Uint8Array && bytes.length === 2 };

    yield { type: "probe", value: base64.btoa("hi") };

    yield { type: "probe", value: typeof cryptoMod.crypto.randomUUID() === "string" };

    let denied = null;
    try {
      await fetchMod.fetch("https://example.com");
    } catch (error) {
      denied = String(error?.message ?? error);
    }
    yield { type: "probe", value: denied };
  }
};
"#;

#[test]
fn web_extensions_load_and_fetch_is_denied_by_default() {
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut run = HarnessRun::start(PROBE.to_owned(), Vec::new(), grant(client), Seed::default());

    let events = drain_transcript(&mut run);
    support::finished(run, "extension probe must finish cleanly");

    let probes: Vec<&serde_json::Value> = events
        .iter()
        .filter(|event| event["type"] == "probe")
        .collect();
    assert_eq!(probes.len(), 5, "one probe per capability: {probes:?}");

    assert_eq!(probes[0]["value"], "c");
    assert_eq!(probes[1]["value"], true);
    assert_eq!(probes[2]["value"], "aGk=");
    assert_eq!(probes[3]["value"], true);
    assert!(
        probes[4]["value"]
            .as_str()
            .is_some_and(|m| m.contains("Requires net")),
        "fetch must fail on the denied net permission, got {:?}",
        probes[4]["value"]
    );
}
