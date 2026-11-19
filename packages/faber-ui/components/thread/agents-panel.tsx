"use client"

import { Bot, MoreHorizontal, Square, LoaderCircle, Circle } from "lucide-react"
import * as React from "react"
import { Link } from "@tanstack/react-router"

import { cn } from "@charlotte-ui/utils"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@charlotte-ui/labs/collapsible"
import { agentName } from "@/components/thread/agents"
import { formatCost, formatTokens } from "@/components/thread/usage-card"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@charlotte-ui/dropdown-menu"
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
    <li data-agent-row={agent.id} className="group rounded-lg hover:bg-muted/60">
      <div className="flex h-9 items-center gap-2">
        {live ? <LoaderCircle aria-label={agent.status} className="size-3.5 shrink-0 animate-spin motion-reduce:animate-none text-muted-foreground" /> : <Circle aria-label={agent.closed ? "Closed" : agent.status} className={cn("size-2 shrink-0 fill-current", agent.status === "failed" ? "text-destructive" : "text-muted-foreground")} />}
        <Link to="/session/$sessionId" params={{ sessionId: agent.id }} aria-label={`Open ${name}'s thread`} className="min-w-0 flex-1 truncate rounded text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring" title={name}>
          {name}
        </Link>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button type="button" aria-label={`Actions for ${name}`} className="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring">
              <MoreHorizontal className="size-4" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-60">
            <div className="space-y-1 px-2 py-2 text-xs text-muted-foreground">
              <p className="font-medium text-foreground">{agent.closed ? "Closed" : agent.status}</p>
              <p>{agent.model}</p>
              {agent.activity ? <p className="break-words">{agent.activity}</p> : null}
              <p>{elapsed === null ? "" : `${formatElapsed(elapsed)} · `}{formatTokens(tokens)} tokens · {agentCost(agent, models)}</p>
            </div>
            {live ? <DropdownMenuItem disabled={stopping} onSelect={() => void stop()}>
              <Square className="size-3" /> {stopping ? "Stopping…" : `Stop ${name}`}
            </DropdownMenuItem> : null}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {error ? <p role="alert" className="pb-1 text-xs text-destructive">{error}</p> : null}
    </li>
  )
}

/** Collapsible agent list for the thread summary, with details in row menus. */
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

  if (rows.length === 0) return null

  return (
    <Collapsible open={open} onOpenChange={setOpen} asChild>
      <section aria-label="Agents" className={cn("min-w-0", className)}>
        <CollapsibleTrigger fullWidth>
          <span className="flex items-center gap-2">
            <Bot className="size-4 shrink-0 text-muted-foreground" />
            <span>Agents ({rows.length})</span>
            <span className="flex gap-2 text-[11px] text-muted-foreground" aria-live="polite">
              <span data-count="running">{running} running</span>
              <span data-count="done" className="sr-only">{done} done</span>
              {queued > 0 ? <span data-count="queued">{queued} queued</span> : null}
            </span>
          </span>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <ul className="flex max-h-[min(28rem,60vh)] flex-col overflow-y-auto">
            {rows.map((agent) => (
              <AgentRow key={agent.id} sessionId={sessionId} agent={agent} models={models} now={now} />
            ))}
          </ul>
        </CollapsibleContent>
      </section>
    </Collapsible>
  )
}
