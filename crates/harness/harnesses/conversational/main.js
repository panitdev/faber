/// <reference path="../types.d.ts" />

// Identity, plus a system prompt, a commit, and a tool loop — what a
// multi-turn conversation with an environment needs.
//
// `identity.js` is the literal form of abstract.md §4's "default is
// identity" and commits nothing, which is correct for what it claims and
// wrong for a conversation: types.d.ts is explicit that history does not
// auto-advance in the current substrate, so "a harness that streams a call
// and never commits it leaves history.read() returning the same thing next
// time." Every turn would then start from an empty lineage.
//
// H4's answer is Core adopting the last request plus its completion by
// default, with commit demoted to the best-of-N override. Until that lands
// this file is the difference, and it should disappear when it does.
//
// The tool loop is here rather than in each harness for the same reason the
// tool surface is not written per harness: a granted tool the loop never
// invokes is a capability the model can see and cannot use, which is worse
// than not granting it. Only the *last* call is committed, and that is
// enough — each call carries every earlier turn by value, so the final one's
// turn list is the whole exchange, tool calls and results included.

async function generateTitle(ctx, input) {
  const history = ctx.history.read();
  if (history.length > 0) return null;

  if (!ctx.functions.available.includes("session.get_title")) return null;

  try {
    const currentTitle = await ctx.functions.invoke("session.get_title", null);
    if (currentTitle !== null) return null;

    const call = ctx.llm.stream({
      messages: [
        {
          role: "system",
          content: [
            {
              type: "text",
              text: "Generate a concise title for the user's prompt. Reply with only the title, with no quotes or explanation.",
            },
          ],
        },
        ...input,
      ],
      tools: [],
      maxTokens: 64,
    });

    for await (const _event of call) {
      // Drain the hidden title stream.
    }
    const completion = await call.completion;
    const title = (completion.message.content ?? [])
      .filter((block) => block.type === "text")
      .map((block) => block.text)
      .join("")
      .trim();

    if (title) {
      // The initial check can become stale while the hidden title request runs.
      // Do not replace a title another run or the user set in the meantime.
      const latestTitle = await ctx.functions.invoke("session.get_title", null);
      if (latestTitle !== null) return null;
      await ctx.functions.invoke("session.set_title", title);
      return title;
    }
  } catch {
    // Title generation is best effort and must not affect the main response.
  }
  return null;
}

function toolCallsIn(message) {
  return message?.role === "assistant"
    ? (message.content ?? []).filter((block) => block.type === "tool_use")
    : [];
}

async function* dispatchToolCalls(ctx, calls, results) {
  for (const use of calls) {
    yield { type: "tool_call", id: use.id, name: use.name, input: use.input };

    // `undefined` means the tool was never granted — a move this loop does
    // not have. Saying so as a tool_result keeps the pairing intact.
    const invocation = ctx.tools.invoke(use.name, use.input);
    const result =
      invocation === undefined
        ? { content: `\`${use.name}\` is not a tool this run was granted`, isError: true }
        : await invocation;

    yield {
      type: "tool_result",
      id: use.id,
      name: use.name,
      content: result.content,
      isError: result.isError,
    };
    results.push({
      type: "tool_result",
      toolUseId: use.id,
      content: result.content,
      isError: result.isError,
    });
  }
}

const SYSTEM_PROMPT = `You are Faber, an agent that does work in the user's bound environments through tools.
\`bound_environments\` lists what you can reach; every other environment tool takes \`execute_in\`, and \`exec\`/\`start\` take \`cwd\`, which applies to that call only and never persists.
\`patch\` operations run in order and are not atomic. A finished command is a result even when its exit is nonzero.
Act with tools when a call answers the question; be direct and concise, and report what you did.`;

export default {
  async *execute(ctx, input) {
    const history = ctx.history.read();

    // The default prompt, sent exactly once. It is committed with the first
    // call, so later turns inherit it through `history` instead of resending
    // it by value — which would read as a changed prefix (`scaffold_mismatch`)
    // and invalidate every cached byte behind it. A thread seeded from before
    // this prompt existed keeps no prompt, for the same reason: rewriting its
    // head would do the same to its cache.
    const messages =
      history.length === 0
        ? [{ role: "system", content: [{ type: "text", text: SYSTEM_PROMPT }] }]
        : [...history];

    // A previous run can leave a committed assistant tool call without its
    // user tool-result turn. Recover it before appending new input; otherwise
    // the first request of this run is malformed and the provider rejects it.
    const pendingCalls = toolCallsIn(messages.at(-1));
    if (pendingCalls.length > 0) {
      const results = [];
      yield* dispatchToolCalls(ctx, pendingCalls, results);
      messages.push({ role: "user", content: results });

      // The results above were produced now, not when the model asked for
      // them — the world may have moved on in between. Say so as a
      // mid-conversation system note rather than leaving the model to assume
      // the results are as fresh as its call.
      messages.push({
        role: "system",
        content: [
          {
            type: "text",
            text: "The previous run was interrupted after these tool calls were made; the results above were produced just now, on resume, and may reflect a state that has changed since the calls were issued.",
          },
        ],
      });
    }
    messages.push(...input);
    let call = null;

    // Start title generation in parallel. It must not delay the first visible
    // model event, but the promise is awaited after the response so the
    // isolate stays alive long enough to persist the title.
    const titlePromise = generateTitle(ctx, input);

    while (true) {
      call = ctx.llm.stream({ messages });
      yield* call;

      const completion = await call.completion;
      const content = completion.message.content ?? [];
      const calls = content.filter((block) => block.type === "tool_use");
      if (calls.length === 0) {
        break;
      }

      // The assistant's turn goes back exactly as it came, tool_use blocks
      // and all: a tool_result whose tool_use is missing or edited is a
      // malformed request, and the pairing is checked before anything is
      // sent.
      messages.push(completion.message);

      const results = [];
      yield* dispatchToolCalls(ctx, calls, results);

      messages.push({ role: "user", content: results });
    }

    // After the loop, so what is adopted is the call the conversation
    // actually ended on. The call is passed whole — it already holds exactly
    // what was sent and exactly what came back (proposal.md §6.1).
    if (call !== null) {
      await ctx.commit(call);
    }

    const title = await titlePromise;
    if (title !== null) {
      yield { type: "session_title", title };
    }
  },
};
