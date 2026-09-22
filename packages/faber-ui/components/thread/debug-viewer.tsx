"use client"

import * as React from "react"
import { motion } from "framer-motion"
import { Bug, X } from "lucide-react"

import {
  faber,
  FaberError,
  type Exchange,
  type ExchangeDetail,
  type JsonValue,
  type Run,
  type TranscriptEvent,
  type Uuid,
} from "@/lib/api"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"

/**
 * The two raw logs behind a conversation, for debugging.
 *
 * They are separate records that neither derives from the other
 * (`history-abstract.md` H2): the transcript is what the user saw — the
 * harness-yielded event stream — and the exchange is what happened at the
 * capability boundary — request bytes and provider events. Switching between
 * them is switching between those two questions, so one surface carries both
 * rather than two dialogs that differ only in which pane starts open.
 */
export type DebugView = "transcript" | "exchange"

export function DebugViewerDialog({
  view,
  threadId,
  onViewChange,
  onClose,
}: {
  view: DebugView
  /** The thread on screen. `null` until the session's root thread resolves. */
  threadId: Uuid | null
  onViewChange: (view: DebugView) => void
  onClose: () => void
}) {
  const { runs, error } = useRuns(threadId)

  // A pane is mounted the first time it is shown and then kept, hidden, so
  // switching back and forth never refetches a log that has not changed.
  const [seen, setSeen] = React.useState<Record<DebugView, boolean>>({
    transcript: view === "transcript",
    exchange: view === "exchange",
  })
  React.useEffect(() => {
    setSeen((current) => (current[view] ? current : { ...current, [view]: true }))
  }, [view])

  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose()
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [onClose])

  const title = view === "transcript" ? "Transcript viewer" : "Exchange viewer"

  return (
    <motion.div
      role="dialog"
      aria-modal="true"
      aria-label={title}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.15, ease: "easeOut" }}
      className="absolute inset-0 z-30 bg-background/80 backdrop-blur-sm"
      onPointerDown={onClose}
    >
      <motion.div
        initial={{ opacity: 0, scale: 0.97, y: 8 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        exit={{ opacity: 0, scale: 0.98, y: 8 }}
        transition={{ type: "spring", stiffness: 380, damping: 30 }}
        className="pointer-events-auto absolute inset-3 flex min-h-0 flex-col overflow-hidden rounded-2xl border border-border bg-background shadow-lg"
        onPointerDown={(event) => event.stopPropagation()}
      >
        <header className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-3">
          <Bug className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          <h2 className="text-sm font-semibold">{title}</h2>
          <div className="ml-auto flex items-center gap-1 rounded-full border border-border p-1">
            <Button
              size="xs"
              variant={view === "transcript" ? "secondary" : "ghost"}
              aria-pressed={view === "transcript"}
              onClick={() => onViewChange("transcript")}
            >
              View transcripts
            </Button>
            <Button
              size="xs"
              variant={view === "exchange" ? "secondary" : "ghost"}
              aria-pressed={view === "exchange"}
              onClick={() => onViewChange("exchange")}
            >
              View exchanges
            </Button>
          </div>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label="Close debug viewer"
            onClick={onClose}
          >
            <X />
          </Button>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          {!threadId ? (
            <p className="text-sm text-muted-foreground">Loading thread…</p>
          ) : error ? (
            <p className="text-sm text-destructive">{error}</p>
          ) : !runs ? (
            <p className="text-sm text-muted-foreground">Loading runs…</p>
          ) : (
            <>
              {seen.transcript ? (
                <div className={cn(view !== "transcript" && "hidden")}>
                  <TranscriptPane runs={runs} />
                </div>
              ) : null}
              {seen.exchange ? (
                <div className={cn(view !== "exchange" && "hidden")}>
                  <ExchangePane runs={runs} />
                </div>
              ) : null}
            </>
          )}
        </div>
      </motion.div>
    </motion.div>
  )
}

/** The thread's runs, oldest first — the spine every debug pane hangs off. */
function useRuns(threadId: Uuid | null): { runs: Run[] | null; error: string | null } {
  const [runs, setRuns] = React.useState<Run[] | null>(null)
  const [error, setError] = React.useState<string | null>(null)

  React.useEffect(() => {
    if (!threadId) return
    let cancelled = false
    setRuns(null)
    setError(null)

    void faber
      .listRuns(threadId)
      .then((rows) => {
        if (!cancelled) setRuns(rows)
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof FaberError ? err.message : "failed to load runs")
        }
      })

    return () => {
      cancelled = true
    }
  }, [threadId])

  return { runs, error }
}

const TRANSCRIPT_PAGE = 500

async function loadAllTranscript(runId: Uuid): Promise<TranscriptEvent[]> {
  let events: TranscriptEvent[] = []
  let afterSeq: number | undefined
  for (;;) {
    const page = await faber.listTranscript(runId, {
      after_seq: afterSeq,
      limit: TRANSCRIPT_PAGE,
    })
    events = [...events, ...page]
    if (page.length < TRANSCRIPT_PAGE) break
    const last = page[page.length - 1]?.seq
    if (last === undefined) break
    afterSeq = last
  }
  return events
}

/** The durable event stream, run by run, exactly as the transcript stored it. */
function TranscriptPane({ runs }: { runs: Run[] }) {
  const [events, setEvents] = React.useState<Record<Uuid, TranscriptEvent[]>>({})
  const [error, setError] = React.useState<string | null>(null)
  const [loading, setLoading] = React.useState(true)

  React.useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    setEvents({})

    async function load() {
      try {
        for (const run of runs) {
          const page = await loadAllTranscript(run.id)
          if (cancelled) return
          setEvents((current) => ({ ...current, [run.id]: page }))
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof FaberError ? err.message : "failed to load the transcript")
        }
      } finally {
        if (!cancelled) setLoading(false)
      }
    }

    void load()
    return () => {
      cancelled = true
    }
  }, [runs])

  if (error) return <p className="text-sm text-destructive">{error}</p>
  if (runs.length === 0) return <Empty>No runs yet.</Empty>

  return (
    <div className="flex flex-col gap-8">
      {runs.map((run, index) => (
        <section key={run.id} className="flex flex-col gap-2">
          <RunHeading index={index} run={run} count={events[run.id]?.length} />
          {(events[run.id] ?? []).map((event) => (
            <EventCard key={event.id} event={event} />
          ))}
          {loading && events[run.id] === undefined ? (
            <p className="text-xs text-muted-foreground">Loading…</p>
          ) : null}
        </section>
      ))}
    </div>
  )
}

function EventCard({ event }: { event: TranscriptEvent }) {
  return (
    <div className="flex flex-col gap-1 rounded-lg border border-border">
      <div className="flex items-center gap-2 border-b border-border px-3 py-1.5 text-xs">
        <span className="font-mono text-muted-foreground">#{event.seq}</span>
        <span className="font-medium">{event.kind}</span>
        <span className="ml-auto text-muted-foreground">{formatTime(event.created_at)}</span>
      </div>
      <JsonBlock value={event.payload} />
    </div>
  )
}

/** What Core observed at the capability boundary, run by run. */
function ExchangePane({ runs }: { runs: Run[] }) {
  const [exchanges, setExchanges] = React.useState<Record<Uuid, Exchange[]>>({})
  const [error, setError] = React.useState<string | null>(null)
  const [loading, setLoading] = React.useState(true)

  React.useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    setExchanges({})

    async function load() {
      try {
        for (const run of runs) {
          const page = await faber.listExchanges(run.id)
          if (cancelled) return
          setExchanges((current) => ({ ...current, [run.id]: page }))
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof FaberError ? err.message : "failed to load exchanges")
        }
      } finally {
        if (!cancelled) setLoading(false)
      }
    }

    void load()
    return () => {
      cancelled = true
    }
  }, [runs])

  if (error) return <p className="text-sm text-destructive">{error}</p>
  if (runs.length === 0) return <Empty>No runs yet.</Empty>

  return (
    <div className="flex flex-col gap-8">
      {runs.map((run, index) => (
        <section key={run.id} className="flex flex-col gap-2">
          <RunHeading index={index} run={run} count={exchanges[run.id]?.length} />
          {(exchanges[run.id] ?? []).map((exchange, position) => (
            <ExchangeCard key={exchange.id} exchange={exchange} position={position} />
          ))}
          {loading && exchanges[run.id] === undefined ? (
            <p className="text-xs text-muted-foreground">Loading…</p>
          ) : null}
        </section>
      ))}
    </div>
  )
}

type DetailState =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "ready"; detail: ExchangeDetail }

function ExchangeCard({ exchange, position }: { exchange: Exchange; position: number }) {
  const [open, setOpen] = React.useState(false)
  const [detail, setDetail] = React.useState<DetailState | null>(null)

  const toggle = () => {
    const next = !open
    setOpen(next)
    if (!next || detail) return

    setDetail({ status: "loading" })
    void faber
      .getExchange(exchange.id)
      .then((loaded) => setDetail({ status: "ready", detail: loaded }))
      .catch((err) => {
        setDetail({
          status: "error",
          message: err instanceof FaberError ? err.message : "failed to load the exchange",
        })
      })
  }

  return (
    <div className="flex flex-col rounded-lg border border-border">
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs hover:bg-muted/40"
      >
        <span className="font-mono text-muted-foreground">#{position}</span>
        <span className="font-medium">{outcomeLabel(exchange.outcome)}</span>
        {exchange.canonical ? <Tag>canonical</Tag> : null}
        {!exchange.has_provider_events ? <Tag>no events</Tag> : null}
        <span className="ml-auto text-muted-foreground">
          {formatTime(exchange.started_at)}
        </span>
      </button>

      {open ? (
        <div className="flex flex-col gap-3 border-t border-border px-3 py-3">
          {detail === null || detail.status === "loading" ? (
            <p className="text-xs text-muted-foreground">Loading…</p>
          ) : detail.status === "error" ? (
            <p className="text-xs text-destructive">{detail.message}</p>
          ) : (
            <>
              <MetaGrid exchange={detail.detail} />
              <JsonBlock label="Request" value={prettyJson(detail.detail.request)} raw />
              {detail.detail.provider_events !== null ? (
                <JsonBlock label="Provider events" value={detail.detail.provider_events} />
              ) : null}
              {detail.detail.canonical_blob !== null ? (
                <JsonBlock label="Canonical lineage" value={detail.detail.canonical_blob} />
              ) : null}
            </>
          )}
        </div>
      ) : null}
    </div>
  )
}

function MetaGrid({ exchange }: { exchange: ExchangeDetail }) {
  const rows: Array<[string, string]> = [
    ["id", exchange.id],
    ["run", exchange.run_id],
    ["cache (expected / actual)", `${exchange.expected_cache_tokens} / ${exchange.actual_cache_tokens ?? "—"}`],
    ["started", formatTime(exchange.started_at)],
    ["completed", exchange.completed_at === null ? "—" : formatTime(exchange.completed_at)],
  ]
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[11px]">
      {rows.map(([label, value]) => (
        <React.Fragment key={label}>
          <dt className="text-muted-foreground">{label}</dt>
          <dd className="truncate font-mono">{value}</dd>
        </React.Fragment>
      ))}
      {exchange.usage !== null ? (
        <>
          <dt className="text-muted-foreground">usage</dt>
          <dd className="font-mono">{JSON.stringify(exchange.usage)}</dd>
        </>
      ) : null}
      {exchange.outcome !== null ? (
        <>
          <dt className="text-muted-foreground">outcome</dt>
          <dd className="font-mono">{JSON.stringify(exchange.outcome)}</dd>
        </>
      ) : null}
    </dl>
  )
}

function RunHeading({ index, run, count }: { index: number; run: Run; count?: number }) {
  return (
    <div className="sticky top-0 z-10 flex flex-wrap items-center gap-2 bg-background/95 py-1 text-xs backdrop-blur-sm">
      <span className="font-semibold">Run {index + 1}</span>
      <span className="font-mono text-muted-foreground">{run.id}</span>
      <span className="text-muted-foreground">
        {run.completed_at === null ? "running" : formatTime(run.completed_at)}
      </span>
      {count !== undefined ? (
        <span className="ml-auto text-muted-foreground">{count} entries</span>
      ) : null}
    </div>
  )
}

function JsonBlock({
  label,
  value,
  raw = false,
}: {
  label?: string
  value: JsonValue | string
  raw?: boolean
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1 px-3 pb-3">
      {label ? <span className="text-xs font-medium text-muted-foreground">{label}</span> : null}
      <pre className="max-h-96 overflow-auto rounded-md border border-border bg-muted/40 p-3 font-mono text-[11px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]">
        {raw || typeof value === "string" ? String(value) : JSON.stringify(value, null, 2)}
      </pre>
    </div>
  )
}

function Tag({ children }: { children: React.ReactNode }) {
  return (
    <span className="rounded-full border border-border px-1.5 py-0.5 text-[10px] text-muted-foreground">
      {children}
    </span>
  )
}

function Empty({ children }: { children: React.ReactNode }) {
  return <p className="text-sm text-muted-foreground">{children}</p>
}

function formatTime(epoch: number): string {
  return new Date(epoch * 1000).toLocaleString()
}

function prettyJson(raw: string): string {
  const trimmed = raw.trim()
  if (!trimmed) return raw
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2)
  } catch {
    return raw
  }
}

function outcomeLabel(outcome: JsonValue | null): string {
  if (outcome && typeof outcome === "object" && !Array.isArray(outcome) && "type" in outcome) {
    const type = (outcome as { type?: JsonValue }).type
    if (typeof type === "string") return type
  }
  return outcome === null ? "unknown" : "outcome"
}
