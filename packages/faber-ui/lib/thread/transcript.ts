/**
 * Turns the wire vocabulary a run yields (`crates/harness/src/mapping.rs`'s
 * `LLMEvent` + the API's `tool_result`/`input`/`run_end`/`run_error`/
 * `run_interrupted`
 * additions, `crates/api/src/run.rs`) into what {@link AgentRun} renders.
 *
 * Fed from two sources that share the same `{kind, payload}` shape —
 * `listTranscript` (durable, per run) and `streamSession` (live, per session)
 * — through the same `applyEvent`, so replay and live share one code path.
 */

import type {
  AgentRunItem,
  AgentRunThinkingItem,
  AgentStepState,
} from "@/components/ui/agent-run"
import type { JsonValue, StreamEvent, TranscriptEvent, Uuid } from "@/lib/api"

// ---------------------------------------------------------------------------
// Wire shapes this module actually reads. Only the fields consumed here are
// modeled — everything else in a payload is ignored, deliberately: `kind` is
// free-form on the wire (history-abstract.md H8.7) and new variants must not
// require a type change to keep compiling.
// ---------------------------------------------------------------------------

/**
 * One whole LLM message. The durable form of everything between a
 * `message_start` and its `message_stop` — see `crates/api/src/compact.rs`.
 */
const KIND_MESSAGE = "message"

/**
 * The session's own note that an environment was added (`crates/api`'s
 * `KIND_ENVIRONMENTS`).
 */
const KIND_ENVIRONMENTS = "environments"

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; signature?: string }
  | { type: "tool_use"; id: string; name: string; input: JsonValue }
  | { type: "tool_result"; toolUseId: string; content: string; isError?: boolean }
  | { type: "unknown"; raw: JsonValue }

export type TranscriptMessage = {
  id?: string
  role: "system" | "user" | "assistant"
  content: ContentBlock[]
}

type BlockStartPayload =
  | { type: "text" }
  | { type: "thinking" }
  | { type: "tool_use"; id: string; name: string }
  | { type: "unknown"; raw: JsonValue }

/**
 * The names here are the harness-facing ones from `crates/harness`'s `Delta`,
 * NOT `crates/llm`'s — the latter calls every variant's payload `content`, and
 * `From<llm::Delta>` renames it on the way out to the isolate. What reaches
 * this reducer is whatever the harness yielded, so this side is the one to
 * match.
 */
type DeltaPayload =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "thinking_signature"; signature: string }
  | { type: "tool_input_json"; partialJson: string }
  | { type: "unknown"; raw: JsonValue }

/** A normalized event, uniform across the replay and live sources. */
export type NormalizedEvent = {
  runId: Uuid
  seq: number
  kind: string
  payload: JsonValue
}

export function fromTranscriptEvent(runId: Uuid, event: TranscriptEvent): NormalizedEvent {
  return { runId, seq: event.seq, kind: event.kind, payload: event.payload }
}

export function fromStreamEvent(event: StreamEvent): NormalizedEvent {
  return { runId: event.run_id, seq: event.seq, kind: event.kind, payload: event.payload }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/**
 * `interrupted` is its own state rather than a flavour of `error`: the run
 * stopped because the user asked it to, and the words it managed to get out
 * before then are a real, if short, answer — not the wreckage of a failure.
 * It is live-only. A reload reads a stopped run back as `done`, because the
 * durable record (`run.completed_at`) says a run is over without saying how
 * it ended (`crates/api/src/run.rs`).
 */
export type RunStatus = "running" | "done" | "error" | "interrupted"

/**
 * A thinking row plus whether its block is still being written.
 *
 * `streaming` is transcript state, not view state: it says the model is still
 * appending to this block, which is what lets the view peek at reasoning while
 * it arrives and fold it away once it stops. The mode itself is the view's —
 * nothing here sets {@link AgentRunThinkingItem.mode}.
 *
 * It ends on whichever comes first of the block's own `block_stop`, the next
 * block opening, or the run ending. The middle one is what makes reasoning
 * fold away when *that reasoning* stops rather than when the run does: a
 * provider whose stream carries no block boundaries has them synthesized from
 * flat deltas, and that synthesis can only close every open block together at
 * the end of the message (`crates/llm/src/openai/wire.rs`'s `close_blocks`).
 * Its `block_stop` is therefore too late to be the only signal. A block
 * starting is not: whatever follows reasoning — text, a tool call — opens one,
 * and reasoning never resumes into the block it left, so a row settled this
 * way can never need to un-settle.
 */
export type ThinkingItem = AgentRunThinkingItem & { streaming: boolean }

/** An `AgentRun` row, with the extra bookkeeping a thinking row carries. */
export type TimelineItem = Exclude<AgentRunItem, AgentRunThinkingItem> | ThinkingItem

export type Turn = {
  runId: Uuid
  /** The user's own turn (`kind: "input"`, seq 0) — not an `AgentRun` row. */
  userContent: ContentBlock[]
  /**
   * What the *session* said this turn, rather than the user or the model —
   * today, that an environment was added. Rendered rather than hidden: the
   * model can see these, and a note one side of the conversation cannot read
   * is not part of a conversation.
   */
  notices: string[]
  items: TimelineItem[]
  status: RunStatus
  errorMessage?: string
}

type BlockEntry =
  | { kind: "message"; itemId: string }
  | { kind: "thinking"; itemId: string }
  | { kind: "tool"; itemId: string; partialJson: string }

/** The LLM message a run is streaming right now, and the items it has produced. */
type OpenMessage = { index: number; itemIds: string[] }

export type TranscriptStore = {
  order: Uuid[]
  byRun: Record<Uuid, Turn>
  /** Open content blocks for the run's *current* LLM message, by block index. */
  blocks: Record<Uuid, Record<number, BlockEntry>>
  /**
   * What a `message` event replaces. The API persists whole messages and
   * streams the deltas that build them (`crates/api/src/compact.rs`), so a
   * live client renders a message twice over: once from its deltas, then once
   * from the compacted event that closes it. The second must land *on* the
   * first rather than beside it.
   */
  open: Record<Uuid, OpenMessage | undefined>
  /** Messages seen per run, which is what gives their items a stable id. */
  messageCount: Record<Uuid, number>
  /** Seqs already applied per run — replay and live overlap at the resume cursor. */
  seen: Record<Uuid, Record<number, true>>
}

export function createStore(): TranscriptStore {
  return { order: [], byRun: {}, blocks: {}, open: {}, messageCount: {}, seen: {} }
}

export function turnsOf(store: TranscriptStore): Turn[] {
  return store.order.map((id) => store.byRun[id])
}

function ensureRun(store: TranscriptStore, runId: Uuid): TranscriptStore {
  if (store.byRun[runId]) return store
  return {
    ...store,
    order: [...store.order, runId],
    byRun: {
      ...store.byRun,
      [runId]: { runId, userContent: [], notices: [], items: [], status: "running" },
    },
    blocks: { ...store.blocks, [runId]: {} },
    messageCount: { ...store.messageCount, [runId]: 0 },
    seen: { ...store.seen, [runId]: {} },
  }
}

/**
 * Opens a message for a run that has none, and returns the store alongside it.
 *
 * A run resumed mid-message never saw its `message_start`, and a harness may
 * yield blocks without one at all — either way the content is real and belongs
 * to a message, so one is opened rather than the blocks dropped.
 */
function openMessage(store: TranscriptStore, runId: Uuid): [TranscriptStore, OpenMessage] {
  const existing = store.open[runId]
  if (existing) return [store, existing]
  const index = (store.messageCount[runId] ?? 0) + 1
  const opened: OpenMessage = { index, itemIds: [] }
  return [
    {
      ...store,
      open: { ...store.open, [runId]: opened },
      messageCount: { ...store.messageCount, [runId]: index },
    },
    opened,
  ]
}

function trackItem(store: TranscriptStore, runId: Uuid, itemId: string): TranscriptStore {
  const open = store.open[runId]
  if (!open) return store
  return {
    ...store,
    open: { ...store.open, [runId]: { ...open, itemIds: [...open.itemIds, itemId] } },
  }
}

function updateRun(store: TranscriptStore, runId: Uuid, patch: Partial<Turn>): TranscriptStore {
  const current = store.byRun[runId]
  return { ...store, byRun: { ...store.byRun, [runId]: { ...current, ...patch } } }
}

function updateItem(
  store: TranscriptStore,
  runId: Uuid,
  itemId: string,
  update: (item: TimelineItem) => TimelineItem,
): TranscriptStore {
  const run = store.byRun[runId]
  const items = run.items.map((item) => (item.id === itemId ? update(item) : item))
  return updateRun(store, runId, { items })
}

function setBlock(
  store: TranscriptStore,
  runId: Uuid,
  index: number,
  entry: BlockEntry | undefined,
): TranscriptStore {
  const runBlocks = { ...store.blocks[runId] }
  if (entry) runBlocks[index] = entry
  else delete runBlocks[index]
  return { ...store, blocks: { ...store.blocks, [runId]: runBlocks } }
}

/** Applies one event, in `seq` order, to a store — pure, returns a new store. */
export function applyEvent(store: TranscriptStore, event: NormalizedEvent): TranscriptStore {
  const { runId, seq, kind, payload } = event

  if (store.seen[runId]?.[seq]) return store // replay/live overlap at the resume cursor

  let next = ensureRun(store, runId)
  next = { ...next, seen: { ...next.seen, [runId]: { ...next.seen[runId], [seq]: true } } }

  switch (kind) {
    case "input": {
      const message = payload as unknown as TranscriptMessage
      return updateRun(next, runId, { userContent: message.content ?? [] })
    }

    /**
     * A system-role message the API wrote alongside the user's, saying an
     * environment was added. The tags around it are for the model; what a
     * reader wants is the sentence inside them.
     */
    case KIND_ENVIRONMENTS: {
      const message = payload as unknown as TranscriptMessage
      const text = (message.content ?? [])
        .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
        .map((block) => block.text.replace(/<\/?environments>/g, "").trim())
        .filter(Boolean)
      const run = next.byRun[runId]
      return updateRun(next, runId, { notices: [...run.notices, ...text] })
    }

    case "message_start": {
      // A fresh LLM message starts a fresh set of block indices.
      const count = (next.messageCount[runId] ?? 0) + 1
      return {
        ...next,
        blocks: { ...next.blocks, [runId]: {} },
        open: { ...next.open, [runId]: { index: count, itemIds: [] } },
        messageCount: { ...next.messageCount, [runId]: count },
      }
    }

    /**
     * One whole LLM message, as the API persists it. On replay this is the
     * only thing a run's model output arrives as; live it lands after the
     * deltas that already drew it, and replaces them.
     */
    case KIND_MESSAGE: {
      const message = payload as unknown as TranscriptMessage
      let open: OpenMessage
      ;[next, open] = openMessage(next, runId)
      const run = next.byRun[runId]

      const replaced = new Set(open.itemIds)
      const at = run.items.findIndex((item) => replaced.has(item.id))
      const kept = run.items.filter((item) => !replaced.has(item.id))
      const previous = new Map(run.items.map((item) => [item.id, item]))
      const rebuilt = itemsFromContent(open.index, message.content ?? [], previous)

      const insertAt = at === -1 ? kept.length : at
      const items = [...kept.slice(0, insertAt), ...rebuilt, ...kept.slice(insertAt)]

      next = updateRun(next, runId, { items })
      return {
        ...next,
        blocks: { ...next.blocks, [runId]: {} },
        open: { ...next.open, [runId]: undefined },
      }
    }

    case "block_start": {
      const { index, block } = payload as unknown as { index: number; block: BlockStartPayload }
      let open: OpenMessage
      ;[next, open] = openMessage(next, runId)
      // Anything opening a block means the model has moved on from whatever
      // reasoning was still open — including an unknown block, which this
      // timeline drops but which is content all the same.
      next = updateRun(next, runId, { items: settleThinking(next, runId) })
      if (block.type === "text" || block.type === "thinking") {
        // Derived from the message and block it renders, not from `seq`, so
        // the compacted `message` event rebuilds the same id and React keeps
        // the row rather than remounting it mid-reveal.
        const itemId = blockItemId(open.index, index)
        const run = next.byRun[runId]
        const item: TimelineItem =
          block.type === "text"
            ? { kind: "message", id: itemId, text: "" }
            : { kind: "thinking", id: itemId, text: "", streaming: true }
        const items: TimelineItem[] = [...run.items, item]
        next = updateRun(next, runId, { items })
        next = trackItem(next, runId, itemId)
        return setBlock(next, runId, index, { kind: item.kind, itemId })
      }
      if (block.type === "tool_use") {
        const run = next.byRun[runId]
        const items: TimelineItem[] = [
          ...run.items,
          { kind: "tool", id: block.id, name: block.name, state: "pending" },
        ]
        next = updateRun(next, runId, { items })
        next = trackItem(next, runId, block.id)
        return setBlock(next, runId, index, { kind: "tool", itemId: block.id, partialJson: "" })
      }
      // Unknown blocks are deliberately not surfaced on the timeline — leave
      // the index unmapped, so their deltas are dropped too.
      return next
    }

    case "block_delta": {
      const { index, delta } = payload as unknown as { index: number; delta: DeltaPayload }
      const entry = next.blocks[runId]?.[index]
      if (!entry) return next // an unmapped (unknown) block
      if (entry.kind === "message" && delta.type === "text") {
        return updateItem(next, runId, entry.itemId, (item) =>
          item.kind === "message" ? { ...item, text: item.text + delta.text } : item,
        )
      }
      // `thinking_signature` carries no reasoning to show — it is the opaque
      // token the provider needs back, and the harness is what keeps it.
      if (entry.kind === "thinking" && delta.type === "thinking") {
        return updateItem(next, runId, entry.itemId, (item) =>
          item.kind === "thinking" ? { ...item, text: item.text + delta.thinking } : item,
        )
      }
      if (entry.kind === "tool" && delta.type === "tool_input_json") {
        const partialJson = entry.partialJson + delta.partialJson
        return setBlock(next, runId, index, { ...entry, partialJson })
      }
      return next
    }

    case "block_stop": {
      const { index } = payload as unknown as { index: number }
      const entry = next.blocks[runId]?.[index]
      if (!entry) return next
      if (entry.kind === "thinking") {
        // The model is done reasoning here, whatever it does next.
        return updateItem(next, runId, entry.itemId, (item) =>
          item.kind === "thinking" ? { ...item, streaming: false } : item,
        )
      }
      if (entry.kind !== "tool") return next
      const input = parseJsonLoosely(entry.partialJson)
      return updateItem(next, runId, entry.itemId, (item) =>
        item.kind === "tool" ? { ...item, state: "running" as AgentStepState, input } : item,
      )
    }

    /**
     * Which tool call this answers is named two ways in the wild. The Rust
     * `TranscriptEvent::ToolResult` writes `toolUseId`, but a harness is free
     * to yield its own presentation events, and the conversational one
     * (`crates/harness/harnesses/conversational.js`) sends `id` — it keeps
     * `toolUseId` for the block it pushes back to the *model*, where the
     * pairing is validated, which is why only this timeline ever noticed.
     * Reading both is the only thing that renders every run already stored.
     *
     * A miss is silent by construction: `updateItem` maps over the run's rows
     * and matches none, leaving the call at `running` — a node stuck as a
     * half-drawn ring with nothing inside it.
     */
    case "tool_result": {
      const { toolUseId, id, content, isError } = payload as unknown as {
        toolUseId?: string
        id?: string
        content: string
        isError?: boolean
      }
      return updateItem(next, runId, toolUseId ?? id ?? "", (item) =>
        item.kind === "tool"
          ? { ...item, state: isError ? "error" : "success", result: content }
          : item,
      )
    }

    case "run_end":
      return updateRun(next, runId, { status: "done", items: settleThinking(next, runId) })

    // Settled like any other ending: a stopped run's reasoning block never
    // gets its `block_stop`, and leaving it open would keep a spinner running
    // for a run that is over.
    case "run_interrupted":
      return updateRun(next, runId, {
        status: "interrupted",
        items: settleThinking(next, runId),
      })

    case "run_error": {
      const message = (payload as { message?: string } | null)?.message
      return updateRun(next, runId, {
        status: "error",
        errorMessage: message,
        // A run that died mid-block never sent the `block_stop` that ends its
        // reasoning, and nothing followed it either; the run being over ends
        // it just as well.
        items: settleThinking(next, runId),
      })
    }

    case "retry": {
      const { attempt, maxRetries, delayMs, error: retryError } =
        payload as unknown as { attempt: number; maxRetries: number; delayMs: number; error: string }
      const run = next.byRun[runId]
      const retryNotice = `Retrying (${attempt}/${maxRetries}) in ${Math.round(delayMs / 1000)}s — ${retryError}`
      return updateRun(next, runId, { notices: [...run.notices, retryNotice] })
    }

    default:
      // message_delta / message_stop and anything future-added carry nothing
      // this timeline renders.
      return next
  }
}

/**
 * Every row of a run, with no thinking block left open.
 *
 * Used both when a run ends and when a new block opens — see
 * {@link ThinkingItem} for why the latter has to count.
 */
function settleThinking(store: TranscriptStore, runId: Uuid): TimelineItem[] {
  return store.byRun[runId].items.map((item) =>
    item.kind === "thinking" && item.streaming ? { ...item, streaming: false } : item,
  )
}

export function applyEvents(store: TranscriptStore, events: NormalizedEvent[]): TranscriptStore {
  return events.reduce(applyEvent, store)
}

/**
 * Items for one whole message's content blocks.
 *
 * `previous` carries whatever the deltas (or an earlier apply) already built,
 * so a tool call that has since resolved keeps its state and result instead of
 * being reset to `running` by the message it belongs to.
 */
function itemsFromContent(
  messageIndex: number,
  content: ContentBlock[],
  previous: Map<string, TimelineItem>,
): TimelineItem[] {
  const items: TimelineItem[] = []
  content.forEach((block, index) => {
    if (block.type === "text") {
      items.push({ kind: "message", id: blockItemId(messageIndex, index), text: block.text })
      return
    }
    if (block.type === "thinking") {
      // The message this block belongs to is closed, so its reasoning is over
      // — on replay that is the only thing a thinking row ever is.
      items.push({
        kind: "thinking",
        id: blockItemId(messageIndex, index),
        text: block.thinking,
        streaming: false,
      })
      return
    }
    if (block.type === "tool_use") {
      const existing = previous.get(block.id)
      items.push({
        kind: "tool",
        id: block.id,
        name: block.name,
        // `running`, not `pending`: the arguments are complete, which is
        // exactly what the live path's `block_stop` says by setting the same.
        //
        // So `pending` is never carried over. A row that streamed in is still
        // `pending` if its `block_stop` never landed, and this message is proof
        // the arguments finished — keeping it would leave the call drawn as an
        // unstarted node for the rest of the session, while the same run after
        // a refresh (no `existing`, so `running`) drew it correctly.
        state:
          existing?.kind === "tool" && existing.state !== "pending"
            ? existing.state
            : "running",
        input: block.input,
        result: existing?.kind === "tool" ? existing.result : undefined,
      })
    }
    // Unknown blocks and tool results are not timeline items — the same
    // omission the delta path makes.
  })
  return items
}

/** Stable across the delta-built item and its compacted replacement. */
function blockItemId(messageIndex: number, blockIndex: number): string {
  return `m${messageIndex}:${blockIndex}`
}

function parseJsonLoosely(raw: string): JsonValue {
  if (!raw.trim()) return {}
  try {
    return JSON.parse(raw) as JsonValue
  } catch {
    return { raw }
  }
}
