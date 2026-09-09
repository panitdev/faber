// The harness-facing surface.
//
// These types are NOT a serde mirror of `crates/llm`. A middle layer builds
// the context injected into the deno_core runtime, and translation is its job:
// field names, tag shapes, and how an absent value is spelled are all free to
// differ from the Rust side. This file is written for the harness author, and
// the Rust side is written for the wire.
//
// What the middle layer cannot do is recover information a type has no room
// to hold. That is the line the notes below track:
//
//   [ops]  the middle layer owns this; a difference from Rust is expected and
//          the note records what the mapping has to preserve.
//   [rust] the crate itself needs a change — the information isn't there, or
//          the value cannot cross the boundary at all.
//
// Fields are camelCase. Discriminant strings stay snake_case: they are
// protocol values echoed from providers, not identifiers.

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

declare type TextContent = {
  type: "text",
  text: string
}

declare type ThinkingContent = {
  type: "thinking",
  thinking: string,
  // Opaque. Must be preserved verbatim when the block is sent back to the
  // same model, so it has to survive the fold, not just the stream.
  signature?: string
}

declare type ToolUseContent = {
  type: "tool_use",
  id: string,
  name: string,
  input: unknown
}

declare type ToolResultContent = {
  type: "tool_result",
  toolUseId: string,
  content: string,
  isError?: boolean
}

// A block type this contract doesn't model yet. Carried through rather than
// dropped: a block that arrives must be one that can be sent back.
declare type UnknownContent = {
  type: "unknown",
  raw: unknown
}

declare type Content =
  | TextContent
  | ThinkingContent
  | ToolUseContent
  | ToolResultContent
  | UnknownContent

declare type LLMRole = "system" | "assistant" | "user"

declare type Message = {
  // Present iff this message belongs to the committed lineage — what
  // `history.read()` hands back carries one; a message a harness is
  // introducing for the first time doesn't.
  readonly id?: string,
  role: LLMRole,
  // Plural. One assistant turn can carry thinking, text, and several parallel
  // tool calls; one user turn carries every tool result at once. No mapping
  // can synthesize this from a single block.
  content: Content[]
}

// A reference with the body omitted — for a harness that doesn't need to
// inspect a committed message and would rather not ship its content back
// across the runtime boundary just to send it unchanged. `read()` still
// returns full `Message`s; this is only for constructing a request.
declare type MessageRef = { readonly id: string }

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

declare type ToolDef = {
  name: string,
  description: string,
  // JSON Schema.
  inputSchema: unknown
}

declare type ToolChoice =
  | { type: "auto" }
  | { type: "any" }
  | { type: "none" }
  | { type: "tool", name: string }

// ---------------------------------------------------------------------------
// Reasoning and sampling
// ---------------------------------------------------------------------------

// Providers that don't reason ignore this.
// [ops] Rust's `Thinking` carries no serde attributes and would encode as
// `{"Adaptive":{...}}` / `"Disabled"`. Irrelevant here — the mapping just has
// to be total in both directions.
declare type Thinking =
  | { type: "adaptive", display: ThinkingDisplay }
  | { type: "disabled" }

declare type ThinkingDisplay =
  // Thinking blocks arrive with empty text.
  | "omitted"
  // Thinking blocks carry a readable summary.
  | "summarized"

// How much the model should spend on a turn. Orthogonal to `Thinking`: a
// harness may want visible reasoning at low effort, or the reverse. A merged
// single knob is a fine convenience to build on top, but it is not the
// contract, because collapsing them loses `ThinkingDisplay`.
declare type Effort = "low" | "medium" | "high" | "xhigh" | "max"

// All optional, and absent means "omit the field" rather than "send the
// default" — current Anthropic models reject a non-default temperature.
declare type Sampling = {
  temperature?: number,
  topP?: number,
  topK?: number
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

// No `model` field, deliberately: roles are an indirection the workflow
// resolves, and a harness that cannot name a model cannot hardcode its
// neighbours. The middle layer supplies it when it builds the `Request`.
//
// `messages` is the only field required, and the only one that doesn't
// default — every other field, when omitted, inherits the value the
// committed lineage was built with (`RequestOptions`, below). That is what
// keeps the zero-configuration call one line *and* correct by default: a
// harness that means to diverge overrides the specific field, and that
// override is legible as a decision.
//
// `sampling` and `stopSequences` distinguish "omitted" from "explicitly
// cleared": `sampling: {}` or `stopSequences: []` clears the inherited value,
// while leaving the field off inherits it unchanged.
//
// `thinking` and `effort` are the one exception to inheriting from the
// lineage: when the caller resolved a reasoning selection for the run (a user
// turning the knob on a session), that selection — and not what an earlier
// turn committed — is what the two fields default to, since it is a choice
// that can change between turns. An explicit value on the call still wins,
// which is how a workflow keeps a cheap side call cheap in an expensive
// conversation.
declare type LLMRequest = {
  // `Turn`, not `Message[]` — see "History and message identity" below for
  // why: an id-bearing turn is a reference to a message Core already holds,
  // resolved and validated (never re-sent from the harness's own copy of its
  // content) before the request is rendered.
  messages: Turn[],
  maxTokens?: number,
  tools?: ToolDef[],
  toolChoice?: ToolChoice,
  thinking?: Thinking,
  effort?: Effort,
  sampling?: Sampling,
  stopSequences?: string[],
  // Merged into the request body verbatim, for provider features this
  // contract has no opinion about yet. Colliding keys are ignored.
  extra?: Record<string, unknown>
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

declare type BlockStart =
  | { type: "text" }
  | { type: "thinking" }
  | { type: "tool_use", id: string, name: string }
  | { type: "unknown", raw: unknown }

// Each variant's payload field is named for the block field it appends to.
//
// [rust] This one does not translate away. Rust's `Delta` is
// `#[serde(tag = "type")]` over newtype variants, and serde rejects that in
// `TaggedSerializer::bad_type` — which is generic over the `Serializer` and
// fails before delegating to any backend. `serde_v8` hits it exactly as
// `serde_json` does, so with `#[op2] #[serde]` ops no `BlockDelta` reaches
// the runtime at all. The variants must become struct variants.
declare type BlockDelta =
  | { type: "text", text: string }
  | { type: "thinking", thinking: string }
  | { type: "thinking_signature", signature: string }
  | { type: "tool_input_json", partialJson: string }
  | { type: "unknown", raw: unknown }

// Surfaced, never acted on: resuming a pause or falling back on a refusal is
// a decision about the shape of the loop, which is the harness's.
// [ops] Rust encodes these externally — `"end_turn"` / `{"other":"x"}`. The
// shape below is the one a TS `switch` wants; mapping between them is
// mechanical.
declare type StopReason =
  | { type: "end_turn" }
  | { type: "max_tokens" }
  | { type: "stop_sequence" }
  | { type: "tool_use" }
  | { type: "pause_turn" }
  | { type: "refusal" }
  | { type: "context_window_exceeded" }
  | { type: "other", content: string }

// Attached to a refusal. Absent for every other stop reason.
declare type StopDetails = {
  category?: string,
  explanation?: string
}

// One provider report. A missing field was absent from that report, which is
// not the same as it being zero — a stream reports input counts up front and
// output counts at the end, so a call's totals are assembled from several of
// these.
//
// [ops] The Rust fields are `Option<u64>` with no `skip_serializing_if`, so a
// naive mapping yields `null`, which these optional fields reject. The
// mapping must omit absent counts rather than null them; that difference is
// the entire point of the type.
declare type UsageDelta = {
  inputTokens?: number,
  outputTokens?: number,
  // Rust: cache_read_input_tokens / cache_creation_input_tokens.
  cacheReadTokens?: number,
  cacheWriteTokens?: number,
  reasoningTokens?: number
}

// An accumulated total, folded from partial reports. A reported zero is a
// real zero — a refusal declined before any output really did produce zero
// output tokens, and recording that as "unreported" would be a lie about what
// happened.
declare type Usage = {
  inputTokens: number,
  outputTokens: number,
  cacheReadTokens: number,
  cacheWriteTokens: number,
  reasoningTokens: number
}

declare type MessageStartEvent = {
  type: "message_start",
  id: string,
  model: string,
  usage: UsageDelta
}

declare type BlockStartEvent = {
  type: "block_start",
  index: number,
  block: BlockStart
}

declare type BlockDeltaEvent = {
  type: "block_delta",
  index: number,
  delta: BlockDelta
}

declare type BlockStopEvent = {
  type: "block_stop",
  index: number
}

declare type MessageDeltaEvent = {
  type: "message_delta",
  stopReason?: StopReason,
  stopDetails?: StopDetails,
  usage: UsageDelta
}

declare type MessageStopEvent = {
  type: "message_stop"
}

// A provider event with no neutral equivalent, passed through raw.
declare type UnknownEvent = {
  type: "unknown",
  raw: unknown
}

declare type LLMEvent =
  | MessageStartEvent
  | BlockStartEvent
  | BlockDeltaEvent
  | BlockStopEvent
  | MessageDeltaEvent
  | MessageStopEvent
  | UnknownEvent

// The result of folding a completed call's events. Not a wire type — it is
// reconstructed on whichever side needs it, which is why the Rust struct
// derives only Clone and Debug and should stay that way.
declare type Completion = {
  id: string,
  model: string,
  message: Message,
  stopReason?: StopReason,
  // Populated only when `stopReason` is `refusal`.
  stopDetails?: StopDetails,
  usage: Usage
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

// Failures are values. Nothing here retries, backs off, or repairs; when to
// try again is the harness's decision, which is why it needs the provider's
// own classification rather than an opaque throw.
//
// [ops] Ops reject with a JS error, so the middle layer must attach this as a
// property rather than flattening it into a message string — a harness that
// has to regex an error text to decide whether to retry has been handed
// nothing.
declare type HarnessError = {
  kind:
    | "transport"
    | "api"
    | "decode"
    | "empty_response"
    | "tool_input"
    | "expected_user_turn"
    // Budget and cancellation surface as ordinary failures, not as a separate
    // channel, so a harness cannot handle one and silently ignore the other.
    | "cancelled"
    | "budget_exhausted"
    // The five below are §7's validation surface — all local, deterministic,
    // and raised at `llm.stream(...)` itself (before anything is sent), not
    // deferred to a stream error.
    //
    // An id-bearing turn (or the id-bearing turns overall) doesn't name a
    // contiguous prefix of the committed lineage starting at index 0 — the
    // one assertion a reference has to satisfy for a provider to have ever
    // cached the bytes it names.
    | "ref_not_prefix"
    // An id-bearing `Message`'s supplied content doesn't match what Core has
    // stored for that id. Distinguishes "I edited this message" (strip the
    // id) from "I'm referencing it unchanged" (keep the id, drop the body,
    // or use a `MessageRef`).
    | "ref_content_mismatch"
    // A structural problem in the resolved turns: a `tool_result` with no
    // preceding `tool_use`, a `tool_use` with no following `tool_result`
    // (except in the final turn), or consecutive system messages after the
    // lineage's leading system run.
    | "malformed_turns"
    // The request references the committed lineage but its tools, or its
    // system prompt, no longer match what that lineage was built with — a
    // harness that means to diverge should build the request without
    // referencing ids from it.
    | "scaffold_mismatch"
    // `commit(call)` on a call that ended with a truncated completion.
    // `commit(call, { partial: true })` opts in — silently adopting a
    // truncation would poison every later turn.
    | "incomplete_completion",
  message: string,
  // Whether the provider classified this as transient. Always `false` for
  // the five validation kinds above — those are local, deterministic facts
  // about the request, not a provider's report.
  transient: boolean,
  status?: number,
  // The provider's request id, for reporting the failure upstream.
  requestId?: string
}

// ---------------------------------------------------------------------------
// History and message identity
// ---------------------------------------------------------------------------
//
// An id names a message Core holds as part of the committed lineage — never
// the bytes any provider rendered from it. `read()` hands back full,
// id-bearing messages; a request can reference one either by sending it back
// unmodified (the id survives) or by naming it bare, as a `MessageRef`.
//
// Sending an *edited* copy of a committed message means constructing a new
// object with no `id` — editing in place and sending it back with the id
// attached is a mismatch Core refuses (`ref_content_mismatch`) rather than a
// choice between the stored and supplied content.

// `Turn` anywhere but a contiguous run at the start of `messages` is an
// ordinary by-value `Message`. An id-bearing turn (either form) is checked
// against the committed lineage at `llm.stream(...)` — see `HarnessError`'s
// `ref_not_prefix` / `ref_content_mismatch` / `scaffold_mismatch`.
declare type Turn = Message | MessageRef

declare type HistoryAccess = {
  // The committed lineage, exact — a clone of what Core holds, not a
  // reconstruction from rendered bytes. Every message carries the id Core
  // minted for it. Returns `[]` on turn one, before anything is committed.
  // Frozen: mutating a returned message doesn't change what's stored, and
  // sending it back after mutating it (with its `id` still attached) is
  // exactly the `ref_content_mismatch` case above.
  read(): Message[]
}

// ---------------------------------------------------------------------------
// Capability
// ---------------------------------------------------------------------------

// `Omit<LLMRequest, "messages">` — every option field, frozen as the
// committed lineage was actually built with it. What `Context.
// committedRequest()` returns, and what every field but `messages` on a new
// request defaults to.
declare type RequestOptions = {
  maxTokens?: number,
  tools: ToolDef[],
  toolChoice?: ToolChoice,
  thinking?: Thinking,
  effort?: Effort,
  sampling?: Sampling,
  stopSequences?: string[],
  extra?: Record<string, unknown>
}

declare type CommitOptions = {
  // Adopts a call that ended with a truncated completion instead of
  // rejecting it. See `HarnessError`'s `incomplete_completion`.
  partial?: boolean
}

// What `llm.stream` returns: the event stream, plus a lazily-resolved handle
// on the finished completion. A getter, not a field one reads once — the
// promise resolves only after the stream completes, and reading it twice
// must not re-drain it.
//
// [ops] `AsyncIterable<LLMEvent> & { completion }` is expressible in TS as an
// intersection; the middle layer's returned object literally has both.
declare type Call = AsyncIterable<LLMEvent> & {
  readonly completion: Promise<Completion>
}

declare type LLM = {
  // The request is not sent until the stream is pulled. It rejects with a
  // `HarnessError`.
  stream(request: LLMRequest): Call
}

declare type ToolAccess = {
  // Exactly what this harness may call — what the workflow withheld simply
  // isn't here.
  available: ToolDef[],
  // `undefined` for a tool that wasn't granted. Not offered is not the same
  // as tried and failed, so an absent tool is a move the loop doesn't have,
  // not an exception to catch.
  invoke(name: string, input: unknown): Promise<ToolResult> | undefined
}

// A tool that ran and reported failure is a successful dispatch carrying an
// error result. A tool that could not be dispatched rejects instead.
declare type ToolResult = {
  content: string,
  isError: boolean
}

// Everything a harness holds, and the only thing it holds. Whatever the
// workflow withholds is unbypassable by construction rather than by policy.
//
// `commit` is attached only when granted — an absent property, not a
// function that throws when called. A harness that never commits loses
// nothing it had.
//
// History does not auto-advance in the current substrate. H4 describes an
// automatic default — "the canonical history for turn N+1 is the final
// request plus its completion" — with `commit` as the *override* for
// best-of-N. What's built today is commit-only: nothing calls `commit` on a
// harness's behalf, so a harness that streams a call and never commits it
// leaves `history.read()` returning the same thing next time. Until the
// workflow-side spine lands (`crates/api`, tracked separately), a harness
// that wants its own answer in its own next-turn history must call
// `commit(call)` itself. `commit` takes the *call*, not a message list — it
// already holds exactly what was sent and what came back, so re-assembling
// that from the harness side would reintroduce the desync ids exist to
// remove. Repeatable within one run; the last commit wins.
//
// [ops] This object is the whole perimeter, so it is also the whole of what
// the middle layer injects. Import control is the sandbox: anything reachable
// by other means is a hole in the design, not a convenience.
declare type Context = {
  llm: LLM,
  tools: ToolAccess,
  history: HistoryAccess,
  // Frozen snapshot, anchored to the committed lineage rather than to
  // chronology: a deliberate off-lineage call (a scratch classification, an
  // escalation probe, a retry at different settings) never becomes the next
  // default-path call's baseline just because it happened most recently.
  committedRequest(): RequestOptions,
  commit?: (call: Call, options?: CommitOptions) => Promise<void>
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

// A `Message` rather than a bare text block, so a harness can be entered with
// an image or a resumed tool result — and so the identity harness needs no
// adaptation:
//
//   export default {
//     execute: (ctx, input) => ctx.llm.stream({ messages: [...ctx.history.read(), input] })
//   } satisfies Harness
declare type Input = Message

// What a harness may yield from `execute`. `LLMEvent`, plus exactly one
// harness-authored addition: a tool result, so a harness that dispatches a
// tool has a way to represent what it did without faking a text block
// (history-abstract.md H9.2). H9.2 stays open beyond that — this is
// deliberately not a wider presentation vocabulary yet.
declare type TranscriptEvent =
  | LLMEvent
  | { type: "tool_result", toolUseId: string, content: string, isError: boolean }

// What the harness yields is what the user sees. What happened is recorded
// separately by Core, from capability use — see harness-events.md.
declare type Harness = {
  execute(ctx: Context, input: Input): AsyncIterable<TranscriptEvent>
}
