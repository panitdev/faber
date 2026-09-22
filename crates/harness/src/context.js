// faber:context.js — builds the capability object a harness receives.
//
// This file is Core's; a harness never imports it directly (see loader.rs).
// A harness's reach is the per-run object this returns, plus the curated
// web-platform globals installed below — abstract.md §4's "no ambient
// authority" holds because both are finite and named: a withheld grant is
// absent from `ctx`, and an extension API not named below is not reachable
// without explicitly loading it with `Deno.core.loadExtScript("ext:...")`.

const ops = Deno.core.ops;

// The curated web-platform surface, installed on `globalThis` before any
// harness module evaluates. The extensions register ops and lazy sources but
// install no globals themselves, so without this a bare `setTimeout` or `URL`
// is a `ReferenceError`. This is the one deliberate, bounded exception to
// "everything through `ctx`": the names below are the whole of it, they match
// the globals `types.d.ts` documents, and everything else an extension offers
// stays opt-in through `Deno.core.loadExtScript` — `fetch` in particular is
// deliberately absent, since it is permission-gated and a harness should have
// to ask for it.
//
// `??=` leaves a name alone if it is already defined, so this is idempotent.
function installGlobals(source, names) {
  const namespace = Deno.core.loadExtScript(source);
  for (const name of names) {
    globalThis[name] ??= namespace[name];
  }
}

installGlobals("ext:deno_web/02_timers.js", [
  "setTimeout",
  "clearTimeout",
  "setInterval",
  "clearInterval",
]);
installGlobals("ext:deno_web/00_url.js", ["URL", "URLSearchParams"]);
installGlobals("ext:deno_web/08_text_encoding.js", ["TextEncoder", "TextDecoder"]);
installGlobals("ext:deno_web/05_base64.js", ["btoa", "atob"]);
installGlobals("ext:deno_crypto/00_crypto.js", ["crypto"]);

// Recursive: `Object.freeze` alone is shallow, and a harness that mutates a
// nested array or object it was handed (`tools.available`, a message's
// `content` array) would be changing state Core still treats as read-only —
// `read()` returning "frozen objects" (proposal.md §3.1) only holds if the
// freeze actually reaches every level.
function deepFreeze(value) {
  if (value === null || typeof value !== "object" || Object.isFrozen(value)) {
    return value;
  }
  for (const key of Object.keys(value)) {
    deepFreeze(value[key]);
  }
  return Object.freeze(value);
}

function llmStream(request) {
  // Not sent until polled (types.d.ts) — op_llm_stream_open renders and
  // validates (a malformed or off-lineage reference rejects here,
  // synchronously) but does not dispatch.
  const rid = ops.op_llm_stream_open(request);
  let completionPromise = null;

  return {
    [Symbol.asyncIterator]() {
      return {
        async next() {
          const event = await ops.op_llm_stream_next(rid);
          if (event === null || event === undefined) {
            return { done: true, value: undefined };
          }
          return { done: false, value: event };
        },
        // No `return`/`throw`: a harness that `break`s out of `for await`
        // leaves this stream's slot `Active`, deliberately — `.completion`
        // must still be legal afterward (it drains silently), and a run's
        // isolate dies at the end of `execute()` regardless, so there is no
        // leak to plug here across runs.
      };
    },
    // A getter, not a field: the promise is minted lazily and cached, so
    // reading `.completion` twice doesn't drain twice.
    get completion() {
      if (completionPromise === null) {
        completionPromise = ops.op_llm_stream_completion(rid);
      }
      return completionPromise;
    },
    // Core-internal — not part of the `Call` type a harness sees in
    // types.d.ts. `commit` needs the stream handle; nothing else does.
    __rid: rid,
  };
}

export function buildContext() {
  const capabilities = ops.op_capabilities();
  const available = deepFreeze(ops.op_tools_available());
  const functions = deepFreeze(ops.op_functions_available());

  const ctx = {
    llm: { stream: llmStream },
    tools: {
      available,
      // `undefined` for a tool that wasn't granted — types.d.ts: not offered
      // is not the same as tried and failed. Checked here, in JS, against
      // the granted list, so the op is never even called for a withheld
      // tool.
      invoke(name, input) {
        if (!available.some((tool) => tool.name === name)) {
          return undefined;
        }
        return ops.op_tool_invoke(name, input);
      },
    },
    history: {
      read: () => deepFreeze(ops.op_history_read()),
    },
    committedRequest: () => deepFreeze(ops.op_committed_request()),
    functions: {
      available: functions,
      invoke: (name, input) => {
        if (!functions.includes(name)) return undefined;
        return ops.op_function_invoke(name, input);
      },
    },
  };

  // Attached only when granted — an absent `commit` is a move the loop
  // doesn't have (abstract.md §4's control by subtraction), not a property
  // that throws when touched.
  if (capabilities.commit) {
    ctx.commit = (call, options) => {
      if (typeof call?.__rid !== "number") {
        throw new TypeError(
          "commit() takes the Call returned by llm.stream(), not a message list or its completion",
        );
      }
      return ops.op_commit(call.__rid, options ?? {});
    };
  }

  return deepFreeze(ctx);
}
