"use client"

import * as React from "react"

import {
  faber,
  FaberError,
  type StreamQuery,
  type TranscriptEvent,
  type Uuid,
} from "@/lib/api"
import {
  applyEvent,
  applyEvents,
  createStore,
  fromStreamEvent,
  fromTranscriptEvent,
  turnsOf,
  type Turn,
  type TranscriptStore,
} from "./transcript"

const TRANSCRIPT_LIMIT = 500
/** Backoff before retrying the live stream after a transient failure (e.g. a network blip). */
const RETRY_DELAY_MS = 1000

export type SessionTranscriptState = {
  turns: Turn[]
  /** True until the thread's durable history has loaded once. */
  loading: boolean
  error: string | null
  /** A run is in flight — drives the prompt box's send/interrupt affordance. */
  isRunning: boolean
  /** The run to interrupt, or `null` when nothing is in flight. */
  runningRunId: Uuid | null
  /**
   * How much prose the run in flight has produced, for deciding whether
   * stopping is worth a confirmation — throwing away a sentence is not the
   * same as throwing away an essay.
   */
  streamedChars: number
}

async function loadHistory(threadId: Uuid): Promise<{ store: TranscriptStore; resume: StreamQuery; title: string | null }> {
  const runs = await faber.listRuns(threadId)
  let store = createStore()
  let resume: StreamQuery = {}
  let title: string | null = null

  for (const run of runs) {
    let events: TranscriptEvent[] = []
    let afterSeq: number | undefined
    for (;;) {
      const page = await faber.listTranscript(run.id, {
        after_seq: afterSeq,
        limit: TRANSCRIPT_LIMIT,
      })
      events = [...events, ...page]
      if (page.length < TRANSCRIPT_LIMIT) break
      afterSeq = page[page.length - 1]?.seq
      if (afterSeq === undefined) break
    }
    store = applyEvents(
      store,
      events.map((event) => fromTranscriptEvent(run.id, event)),
    )
    for (const event of events) {
      if (event.kind !== "session_title") continue
      const payload = event.payload
      if (typeof payload === "object" && payload !== null && "title" in payload && typeof payload.title === "string") {
        title = payload.title
      }
    }
    // New runs persist their terminal marker in the transcript. Keep the
    // completed_at fallback for runs created before terminal markers became
    // durable.
    const hasTerminal = events.some(
      (event) =>
        event.kind === "run_end" ||
        event.kind === "run_error" ||
        event.kind === "run_interrupted",
    )
    if (run.completed_at !== null && !hasTerminal) {
      store = applyEvent(store, {
        runId: run.id,
        seq: -1,
        kind: "run_end",
        payload: null,
      })
    }
    if (run.completed_at === null) {
      const last = events[events.length - 1]
      resume = last ? { run_id: run.id, after_seq: last.seq } : {}
    }
  }

  return { store, resume, title }
}

/**
 * Everything happening in a session's root thread, live. Loads the durable
 * history first, then opens `streamSession` at the resume cursor so the
 * window between fetch and subscribe leaves no gap — a `lagged` disconnect
 * (or any other drop) refetches history and reopens rather than leaving a
 * hole in the transcript.
 *
 * Fork/multi-thread support is deliberately out of scope: this always follows
 * the session's root thread.
 *
 * Assumes `sessionId` is stable for the life of the calling component (`key`
 * it on `sessionId` at the call site) — `threadId` is expected to go from
 * `null` to set exactly once, so there is no mid-life reset to perform.
 */
export function useSessionTranscript(
  sessionId: Uuid | null,
  threadId: Uuid | null,
  onSessionTitle?: (title: string) => void,
): SessionTranscriptState {
  const [store, setStore] = React.useState<TranscriptStore>(() => createStore())
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState<string | null>(null)

  React.useEffect(() => {
    if (!sessionId || !threadId) return

    let cancelled = false
    const controller = new AbortController()

    async function run() {
      const initial = await loadHistory(threadId!)
      if (cancelled) return
      if (initial.title !== null) onSessionTitle?.(initial.title)
      setStore(initial.store)
      setLoading(false)

      let cursor = initial.resume
      while (!cancelled) {
        try {
          for await (const raw of faber.streamSession(sessionId!, cursor, controller.signal)) {
            if (cancelled) return
            if (raw.kind === "session_title" && typeof raw.payload === "object" && raw.payload !== null && "title" in raw.payload && typeof raw.payload.title === "string") {
              onSessionTitle?.(raw.payload.title)
            }
            setStore((prev) => applyEvent(prev, fromStreamEvent(raw)))
          }
          return // the generator returned — an intentional close (abort)
        } catch {
          if (cancelled || controller.signal.aborted) return
          await new Promise((resolve) => setTimeout(resolve, RETRY_DELAY_MS))
          if (cancelled) return
          const refreshed = await loadHistory(threadId!)
          if (cancelled) return
          if (refreshed.title !== null) onSessionTitle?.(refreshed.title)
          setStore(refreshed.store)
          cursor = refreshed.resume
        }
      }
    }

    run().catch((err) => {
      if (cancelled) return
      setError(err instanceof FaberError ? err.message : "failed to load the thread")
      setLoading(false)
    })

    return () => {
      cancelled = true
      controller.abort()
    }
  }, [sessionId, threadId, onSessionTitle])

  const turns = React.useMemo(() => turnsOf(store), [store])
  // The newest, because that is the one a stop means: a session cannot start a
  // second run while one is in flight, but a reconnect can briefly leave an
  // older run looking unfinished.
  const running = React.useMemo(
    () => [...turns].reverse().find((turn) => turn.status === "running") ?? null,
    [turns],
  )

  // Reasoning counts alongside the reply. It is text the user watched arrive
  // and is about to lose, and a long think followed by a first sentence is
  // exactly the case where a misfired stop stings most.
  const streamedChars = React.useMemo(
    () =>
      (running?.items ?? []).reduce(
        (total, item) => total + (item.kind === "tool" ? 0 : item.text.length),
        0,
      ),
    [running],
  )

  return {
    turns,
    loading,
    error,
    isRunning: running !== null,
    runningRunId: running?.runId ?? null,
    streamedChars,
  }
}
