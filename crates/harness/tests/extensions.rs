//! The curated web-platform globals, and the opt-in extension surface.
//!
//! Core installs a fixed set of web globals (timers, URL, text encoding,
//! base64, crypto) before any harness module evaluates — see `context.js`.
//! Those names are the whole of what is ambient; every other extension API is
//! reached opt-in through `Deno.core.loadExtScript("ext:...")`. `fetch` proves
//! both halves: it is deliberately not installed, and loading it still fails
//! on the denied net permission.

mod support;

use std::sync::Arc;

use harness::{HarnessRun, Seed};
use support::{Scripted, drain_transcript, grant, text_reply};

const PROBE: &str = r#"
export default {
  async *execute() {
    const parsed = new URL("https://example.com/a?b=c");
    const bytes = new TextEncoder().encode("hi");

    yield {
      type: "globals",
      types: {
        setTimeout: typeof setTimeout,
        clearTimeout: typeof clearTimeout,
        setInterval: typeof setInterval,
        clearInterval: typeof clearInterval,
        URL: typeof URL,
        URLSearchParams: typeof URLSearchParams,
        TextEncoder: typeof TextEncoder,
        TextDecoder: typeof TextDecoder,
        btoa: typeof btoa,
        atob: typeof atob,
        crypto: typeof crypto,
        // Deliberately not installed — the one name asserted to be absent.
        fetch: typeof fetch,
      },
      query: parsed.searchParams.get("b"),
      roundTrip: new TextDecoder().decode(bytes),
      base64: btoa("hi"),
      uuid: typeof crypto.randomUUID() === "string",
    };

    // The other half of the contract: an uninstalled API is still reachable by
    // asking for it, and a permission-gated one still fails closed.
    const fetchMod = Deno.core.loadExtScript("ext:deno_fetch/26_fetch.js");
    let denied = null;
    try {
      await fetchMod.fetch("https://example.com");
    } catch (error) {
      denied = String(error?.message ?? error);
    }
    yield { type: "fetch", denied };
  }
};
"#;

#[test]
fn core_installs_the_curated_globals_and_fetch_is_opt_in_and_denied() {
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut run = HarnessRun::start(PROBE.to_owned(), Vec::new(), grant(client), Seed::default());

    let events = drain_transcript(&mut run);
    support::finished(run, "extension probe must finish cleanly");

    let globals = events
        .iter()
        .find(|event| event["type"] == "globals")
        .expect("the probe yields its global surface");

    for name in [
        "setTimeout",
        "clearTimeout",
        "setInterval",
        "clearInterval",
        "URL",
        "URLSearchParams",
        "TextEncoder",
        "TextDecoder",
        "btoa",
        "atob",
    ] {
        assert_eq!(
            globals["types"][name], "function",
            "`{name}` must be installed as a global"
        );
    }
    assert_eq!(
        globals["types"]["crypto"], "object",
        "`crypto` must be installed as a global"
    );
    assert_eq!(
        globals["types"]["fetch"], "undefined",
        "`fetch` must stay opt-in, not ambient"
    );

    // The installed globals actually work, not merely exist.
    assert_eq!(globals["query"], "c");
    assert_eq!(globals["roundTrip"], "hi");
    assert_eq!(globals["base64"], "aGk=");
    assert_eq!(globals["uuid"], true);

    let fetch = events
        .iter()
        .find(|event| event["type"] == "fetch")
        .expect("the probe reports its opt-in fetch attempt");
    assert!(
        fetch["denied"]
            .as_str()
            .is_some_and(|m| m.contains("Requires net")),
        "fetch must fail on the denied net permission, got {:?}",
        fetch["denied"]
    );
}
