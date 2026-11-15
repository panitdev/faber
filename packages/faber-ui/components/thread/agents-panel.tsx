"use client"

import { Bot, ChevronDown, Square } from "lucide-react"
import * as React from "react"
import { Link } from "@tanstack/react-router"

import { cn } from "@charlotte-ui/utils"
import { AgentStatusChip, agentName } from "@/components/thread/agents"
import { formatCost, formatTokens } from "@/components/thread/usage-card"
import { SquircleFrame } from "@/components/util/squircle-frame"
import { faber, FaberError, type ModelConfig, type SessionAgent, type Uuid } from "@/lib/api"
import { costOf, pricingOf } from "@/lib/models/pricing"

/** `42s`, `3m 05s`, `1h 12m`. */
export function formatElapsed(seconds: number): string {
  const total = Math.max(0, Math.round(seconds))
  if (total < 60) return `${total}s`
  const minutes = Math.floor(total / 60)
  if (minutes < 60) return `${minutes}m ${String(total % 60).padStart(2, "0")}s`
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`
}

/** What an agent's runs cost: the providers' figure where they gave it all, else priced. */
export function agentCost(agent: SessionAgent, models: readonly ModelConfig[]): string {
  const { usage } = agent
  if (usage.costed && usage.cost !== null) return formatCost(usage.cost)
  const model = models.find((row) => row.alias === agent.model)
  const priced = costOf(
    {
      inputTokens: usage.input_tokens,
      outputTokens: usage.output_tokens,
      cacheReadTokens: usage.cache_read_tokens,
      cacheWriteTokens: usage.cache_write_tokens,
    },
    pricingOf(model),
  )
  const cost = priced ?? usage.cost
  return cost === null ? "—" : `≈${formatCost(cost)}`
}

/** The current time in epoch seconds, ticking while `live`. */
function useNow(live: boolean): number {
  const [now, setNow] = React.useState(() => Date.now() / 1000)
  React.useEffect(() => {
    if (!live) return
    const timer = setInterval(() => setNow(Date.now() / 1000), 1_000)
    return () => clearInterval(timer)
  }, [live])
  return now
}

function AgentRow({
  sessionId,
  agent,
  models,
  now,
}: {
  sessionId: Uuid
  agent: SessionAgent
  models: readonly ModelConfig[]
  now: number
}) {
  const [stopping, setStopping] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)
  const name = agentName(agent)
  const live = !agent.closed && (agent.status === "running" || agent.status === "queued")
  const elapsed =
    agent.started_at === null ? null : (agent.ended_at ?? (live ? now : agent.started_at)) - agent.started_at
  const tokens = agent.usage.input_tokens + agent.usage.output_tokens

  const stop = async () => {
    setStopping(true)
    setError(null)
    try {
      await faber.stopAgent(sessionId, agent.id)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to stop the agent")
    } finally {
      setStopping(false)
    }
  }

  return (
    <li data-agent-row={agent.id} className="group relative flex flex-col gap-0.5 rounded-lg px-2 py-1.5 hover:bg-muted/60">
      <Link
        to="/session/$sessionId"
        params={{ sessionId: agent.id }}
        aria-label={`Open ${name}'s thread`}
        className="absolute inset-0 z-0 rounded-lg outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
      />
      <div className="pointer-events-none relative z-10 flex items-center gap-1.5">
        <span className="truncate text-sm font-medium text-foreground">{name}</span>
        {agent.type && agent.type !== name ? (
          <span className="truncate text-xs text-muted-foreground">{agent.type}</span>
        ) : null}
        <span className="flex-1" />
        <AgentStatusChip agent={agent} />
        {live ? (
          <button
            type="button"
            onClick={() => void stop()}
            disabled={stopping}
            aria-label={`Stop ${name}`}
            className="pointer-events-auto flex size-5 items-center justify-center rounded-md text-muted-foreground hover:bg-background hover:text-foreground disabled:opacity-50"
          >
            <Square className="size-3" />
          </button>
        ) : null}
      </div>
      {agent.activity ? (
        <p className="pointer-events-none relative z-10 truncate text-xs text-muted-foreground">{agent.activity}</p>
      ) : null}
      <p className="pointer-events-none relative z-10 flex gap-3 font-mono text-[11px] text-muted-foreground">
        {elapsed === null ? null : <span>{formatElapsed(elapsed)}</span>}
        <span>{formatTokens(tokens)} tok</span>
        <span>{agentCost(agent, models)}</span>
      </p>
      {error ? <p className="relative z-10 text-xs text-destructive">{error}</p> : null}
    </li>
  )
}

/**
 * The agents a session spawned, at a glance: how many are running, done and
 * queued, then a row each with its status, what it is doing, how long it has
 * taken and what it has used. A row opens the agent's thread; a running one
 * can be stopped. Nothing is drawn until the session has an agent.
 */
export function AgentsPanel({
  sessionId,
  agents,
  models,
  className,
}: {
  sessionId: Uuid
  agents: ReadonlyMap<Uuid, SessionAgent>
  models: readonly ModelConfig[]
  className?: string
}) {
  const [open, setOpen] = React.useState(true)
  const rows = React.useMemo(
    () => [...agents.values()].sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id)),
    [agents],
  )
  const count = (pick: (agent: SessionAgent) => boolean) => rows.filter(pick).length
  const running = count((agent) => !agent.closed && agent.status === "running")
  const queued = count((agent) => !agent.closed && agent.status === "queued")
  const done = rows.length - running - queued
  const now = useNow(running + queued > 0)
  const listId = React.useId()

  if (rows.length === 0) return null

  return (
    <SquircleFrame
      cornerRadius={16}
      borderWidth={1}
      cornerSmoothing={1}
      className={cn("shadow-[0_10px_30px_rgba(0,0,0,0.05)]", className)}
      faceClassName="bg-background/85 backdrop-blur-sm"
    >
      <section aria-label="Agents" className="flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-1 px-2 py-2">
        <button
          type="button"
          aria-expanded={open}
          aria-controls={listId}
          onClick={() => setOpen((value) => !value)}
          className="flex items-center gap-2 rounded-lg px-2 py-1 text-left hover:bg-muted/60"
        >
          <Bot className="size-4 text-muted-foreground" />
          <span className="text-sm font-medium text-foreground">Agents</span>
          <span className="flex flex-1 justify-end gap-2 text-[11px] text-muted-foreground" aria-live="polite">
            <span data-count="running">{running} running</span>
            <span data-count="done">{done} done</span>
            {queued > 0 ? <span data-count="queued">{queued} queued</span> : null}
          </span>
          <ChevronDown className={cn("size-4 text-muted-foreground transition-transform", !open && "-rotate-90")} />
        </button>
        {open ? (
          <ul id={listId} className="flex max-h-[min(28rem,60vh)] flex-col overflow-y-auto">
            {rows.map((agent) => (
              <AgentRow key={agent.id} sessionId={sessionId} agent={agent} models={models} now={now} />
            ))}
          </ul>
        ) : null}
      </section>
    </SquircleFrame>
  )
}
