/**
 * A pretend harness: what the API would do after `sendMessage` returns.
 *
 * It streams the same event vocabulary `crates/api` does — `message_start`,
 * `block_start` / `block_delta` / `block_stop`, then the compacted `message`,
 * `tool_result`, and a terminal marker — so the live path in
 * `lib/thread/transcript.ts` is exercised for real, not a shortcut around it.
 * Only the durable kinds are persisted, as the API does, which keeps replay
 * (reload mid-run, or the resume cursor) honest too.
 *
 * What it answers depends on keywords in the message, so a story or a person
 * clicking around can reach each branch:
 *
 * - contains "error" / "fail"   → reasons briefly, then `run_error`
 * - contains "run" / "ls" / "test" / "build" / "tool" → calls `exec`, then summarizes
 * - anything else               → reasons, then answers in markdown
 */

import type { JsonValue, Run, StreamEvent, TranscriptEvent, Uuid } from "@/lib/api"

import { db, mockSettings, nowEpoch, uuid } from "./db"

// ---------------------------------------------------------------------------
// Session event bus — what `GET /api/sessions/:id/stream` subscribers hear
// ---------------------------------------------------------------------------

type Listener = (event: StreamEvent) => void
const listeners = new Map<Uuid, Set<Listener>>()

export function subscribe(sessionId: Uuid, listener: Listener): () => void {
  let set = listeners.get(sessionId)
  if (!set) {
    set = new Set()
    listeners.set(sessionId, set)
  }
  set.add(listener)
  return () => {
    set.delete(listener)
  }
}

function broadcast(sessionId: Uuid, event: StreamEvent) {
  for (const listener of listeners.get(sessionId) ?? []) {
    try {
      listener(event)
    } catch {
      // A subscriber whose stream closed under it; the stream cleans itself up.
    }
  }
}

/** Runs in flight, by id. Cleared on reset so a stale run never writes into a fresh db. */
const active = new Map<Uuid, { interrupted: boolean }>()

export function resetAgent() {
  for (const run of active.values()) run.interrupted = true
  active.clear()
}

export function isRunActive(runId: Uuid) {
  return active.has(runId)
}

export function interrupt(runId: Uuid) {
  const run = active.get(runId)
  if (run) run.interrupted = true
}

// ---------------------------------------------------------------------------
// Scripts
// ---------------------------------------------------------------------------

type Block =
  | { type: "thinking"; text: string }
  | { type: "text"; text: string }
  | { type: "tool_use"; name: string; input: JsonValue; result: string; isError?: boolean }

type Script = { messages: Block[][]; error?: string }

function scriptFor(content: string): Script {
  const lower = content.toLowerCase()
  const subject = content.replace(/@\S+/g, "").trim().slice(0, 80) || "that"

  if (/\berror\b|\bfail/.test(lower)) {
    return {
      messages: [[{ type: "thinking", text: "Checking the provider before answering…" }]],
      error: "provider returned 529: overloaded (simulated by the Storybook mock)",
    }
  }

  if (/\b(run|ls|test|build|tool|exec)\b/.test(lower)) {
    return {
      messages: [
        [
          { type: "thinking", text: "Easiest to look at the workspace directly rather than guess what is in it." },
          { type: "text", text: "Let me take a look." },
          {
            type: "tool_use",
            name: "exec",
            input: { command: "ls -la" },
            result:
              "total 24\ndrwxr-xr-x  6 geon geon 4096 .\n-rw-r--r--  1 geon geon  812 Cargo.toml\ndrwxr-xr-x  8 geon geon 4096 crates\ndrwxr-xr-x  3 geon geon 4096 packages",
          },
        ],
        [
          {
            type: "text",
            text: `The workspace has a Rust side (\`crates/\`) and the UI under \`packages/\`. Nothing looks out of place for **${subject}**.`,
          },
        ],
      ],
    }
  }

  return {
    messages: [
      [
        {
          type: "thinking",
          text: "This is a mocked reply — no model is being called. Streaming a few blocks so the thread UI has something real to render.",
        },
        {
          type: "text",
          text:
            `You asked: *${subject}*\n\nThis answer comes from the Storybook mock API, streamed as deltas like a real run:\n\n` +
            "- reasoning arrives first and folds away when it ends\n- text streams in word by word\n- usage lands with the compacted message\n\n" +
            "```ts\nconst reply = await faber.sendMessage(sessionId, { content })\n```\n\n" +
            'Say "run ls" to see a tool call, or include "error" to see a failed run.',
        },
      ],
    ],
  }
}

// ---------------------------------------------------------------------------
// Running one
// ---------------------------------------------------------------------------

function chunks(text: string): string[] {
  // Word-ish pieces, keeping whitespace attached, so markdown streams naturally.
  return text.match(/\S+\s*|\s+/g) ?? [text]
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

/** Titles the session from its first message, the way the API's titler would. */
function titleFrom(content: string): string {
  const words = content.replace(/@\S+/g, "").replace(/[`*_#>]/g, "").trim().split(/\s+/).slice(0, 7)
  const title = words.join(" ")
  return title.length > 0 ? title[0].toUpperCase() + title.slice(1) : "New thread"
}

export type StartRunOptions = {
  sessionId: Uuid
  threadId: Uuid
  content: string
  model: string
  /** Environment notices to emit after the input, e.g. an `@` just bound. */
  notices?: string[]
}

/**
 * Registers a run and starts streaming it in the background. Returns as soon
 * as the run exists — like the API's `202`.
 */
export function startRun(options: StartRunOptions): Run {
  const { threadId, model } = options
  const run: Run = { id: uuid(), thread_id: threadId, created_at: nowEpoch(), completed_at: null }
  db().runs.push(run)
  db().transcripts[run.id] = []

  const thread = db().threads.find((row) => row.id === threadId)
  if (thread) thread.next_seq += 1

  const state = { interrupted: false }
  active.set(run.id, state)

  void play(run, state, options, model).finally(() => {
    active.delete(run.id)
  })
  return run
}

async function play(
  run: Run,
  state: { interrupted: boolean },
  { sessionId, content, notices = [] }: StartRunOptions,
  model: string,
) {
  // Captured once: a reset swaps the db out, and a run from the previous story
  // must not write into the next one.
  const database = db()
  const speed = mockSettings().replySpeed
  const pause = (ms: number) => sleep(ms * speed)

  let seq = 0
  const emit = (kind: string, payload: JsonValue, persist: boolean) => {
    if (database !== db()) return
    const event: StreamEvent = { run_id: run.id, seq: seq++, kind, payload }
    if (persist) {
      const durable: TranscriptEvent = {
        id: uuid(),
        seq: event.seq,
        kind,
        payload,
        created_at: nowEpoch(),
      }
      database.transcripts[run.id]?.push(durable)
    }
    broadcast(sessionId, event)
  }

  const finish = (kind: "run_end" | "run_error" | "run_interrupted", payload: JsonValue) => {
    emit(kind, payload, true)
    run.completed_at = nowEpoch()
  }

  emit("input", { role: "user", content: [{ type: "text", text: content }] }, true)
  for (const notice of notices) {
    emit(
      "environments",
      { role: "system", content: [{ type: "text", text: `<environments>${notice}</environments>` }] },
      true,
    )
  }

  const session = database.sessions.find((row) => row.id === sessionId)
  if (session && session.title === null) {
    const title = titleFrom(content)
    session.title = title
    emit("session_title", { title }, true)
  }

  await pause(350)
  const script = scriptFor(content)

  for (const [messageIndex, blocks] of script.messages.entries()) {
    if (state.interrupted) return finish("run_interrupted", null)
    emit("message_start", {}, false)

    const finalContent: JsonValue[] = []
    const toolResults: { id: string; result: string; isError: boolean }[] = []

    for (const [index, block] of blocks.entries()) {
      if (state.interrupted) return finish("run_interrupted", null)

      if (block.type === "tool_use") {
        const id = `toolu_mock_${run.id.slice(0, 8)}_${messageIndex}_${index}`
        emit("block_start", { index, block: { type: "tool_use", id, name: block.name } }, false)
        const json = JSON.stringify(block.input)
        for (let at = 0; at < json.length; at += 12) {
          emit("block_delta", { index, delta: { type: "tool_input_json", partialJson: json.slice(at, at + 12) } }, false)
          await pause(25)
        }
        emit("block_stop", { index }, false)
        finalContent.push({ type: "tool_use", id, name: block.name, input: block.input })
        toolResults.push({ id, result: block.result, isError: block.isError ?? false })
        continue
      }

      emit("block_start", { index, block: { type: block.type } }, false)
      for (const piece of chunks(block.text)) {
        if (state.interrupted) return finish("run_interrupted", null)
        const delta: JsonValue =
          block.type === "thinking" ? { type: "thinking", thinking: piece } : { type: "text", text: piece }
        emit("block_delta", { index, delta }, false)
        await pause(block.type === "thinking" ? 20 : 35)
      }
      emit("block_stop", { index }, false)
      finalContent.push(
        block.type === "thinking"
          ? { type: "thinking", thinking: block.text, signature: "mock" }
          : { type: "text", text: block.text },
      )
    }

    emit("message_stop", {}, false)
    emit(
      "message",
      {
        role: "assistant",
        content: finalContent,
        model,
        usage: {
          inputTokens: 9_800 + messageIndex * 1_200,
          outputTokens: 180 + Math.round(JSON.stringify(finalContent).length / 4),
          cacheReadTokens: 8_400,
          cacheWriteTokens: 600,
          reasoningTokens: 60,
          cost: 0.0142,
        },
      },
      true,
    )

    for (const tool of toolResults) {
      await pause(500) // the tool "running"
      if (state.interrupted) return finish("run_interrupted", null)
      emit("tool_result", { toolUseId: tool.id, content: tool.result, isError: tool.isError }, true)
    }
  }

  if (script.error) return finish("run_error", { message: script.error })
  finish("run_end", null)
}
