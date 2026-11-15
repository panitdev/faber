import { Bot, ExternalLink } from "lucide-react"
import * as React from "react"
import { Link } from "@tanstack/react-router"

import type { AgentRunToolItem, AgentToolRender } from "@charlotte-ui/labs/agent-run"
import { cn } from "@charlotte-ui/utils"
import { Markdown } from "@/components/thread/markdown"
import type { SessionAgent, SessionAgentStatus, Uuid } from "@/lib/api"
import type { TurnInput } from "@/lib/thread/transcript"

/**
 * The agents of the session on screen, by id, as its agents feed last said.
 * Tool rows and reports read their agent's name and live status from here.
 */
export const SessionAgentsContext = React.createContext<ReadonlyMap<Uuid, SessionAgent>>(new Map())

/** The `source` of a message another agent of the project sent. */
export const AGENT_SOURCE = "agent"

const SPAWN = "agent__spawn"
const SEND = "agent__send"
const STOP = "agent__stop"
const LIST = "agent__list"

const STATUS_LABEL: Record<SessionAgentStatus, string> = {
  queued: "Queued",
  running: "Running",
  idle: "Done",
  failed: "Failed",
  stopped: "Stopped",
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function str(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined
}

function oneLine(text: string | undefined): string | undefined {
  return text?.split(/\s+/).join(" ")
}

/** The id `agent__spawn` answered with, once it has. */
function spawnedId(result: unknown): Uuid | undefined {
  if (typeof result !== "string") return undefined
  try {
    return str(asObject(JSON.parse(result))?.id) as Uuid | undefined
  } catch {
    return undefined
  }
}

/** What an agent is called: what its parent named it, else its type. */
export function agentName(agent: Pick<SessionAgent, "name" | "type"> | undefined, fallback = "agent") {
  return agent?.name?.trim() || agent?.type?.trim() || fallback
}

export function AgentStatusChip({
  agent,
  className,
}: {
  agent: Pick<SessionAgent, "status" | "closed">
  className?: string
}) {
  const label = agent.closed ? "Closed" : STATUS_LABEL[agent.status]
  return (
    <span
      data-status={agent.closed ? "closed" : agent.status}
      className={cn(
        "inline-flex shrink-0 items-center gap-1 rounded-full border px-1.5 py-px text-[10px] leading-4 text-muted-foreground",
        agent.status === "running" && !agent.closed && "border-primary/40 text-foreground",
        agent.status === "failed" && "border-destructive/40 text-destructive",
        className,
      )}
    >
      {agent.status === "running" && !agent.closed ? (
        <span className="size-1.5 animate-pulse rounded-full bg-primary" aria-hidden />
      ) : null}
      {label}
    </span>
  )
}

function OpenAgent({ id }: { id: Uuid }) {
  return (
    <Link
      to="/session/$sessionId"
      params={{ sessionId: id }}
      className="inline-flex items-center gap-1 text-sm text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
    >
      <ExternalLink className="size-3.5" />
      Open its thread
    </Link>
  )
}

/**
 * How a call of one of the agent tools reads on the timeline, or `null` for
 * any other tool. A spawn is the agent's card: its name, type and live
 * status, the task folded away, and a way into its thread. A send or a stop
 * is named after the agent it reached.
 */
export function agentToolRender(
  item: AgentRunToolItem,
  agents: ReadonlyMap<Uuid, SessionAgent>,
): AgentToolRender | null {
  const input = asObject(item.input)
  switch (item.name) {
    case SPAWN: {
      const id = spawnedId(item.result)
      const agent = id ? agents.get(id) : undefined
      const type = str(input?.type)
      const name = agent ? agentName(agent) : (str(input?.name) ?? type ?? "agent")
      const prompt = str(input?.prompt)
      return {
        icon: Bot,
        title: name,
        meta: (
          <span className="flex min-w-0 items-center gap-1.5">
            {agent ? <AgentStatusChip agent={agent} /> : null}
            {type && type !== name ? <span className="truncate">{type}</span> : null}
            {agent?.activity ? <span className="truncate">{agent.activity}</span> : null}
          </span>
        ),
        description: prompt ? <Markdown text={prompt} softBreaks /> : undefined,
        // The card, not the JSON the tool answered with.
        children: id ? <OpenAgent id={id} /> : undefined,
      }
    }
    case SEND:
    case STOP: {
      const id = str(input?.id) as Uuid | undefined
      const agent = id ? agents.get(id) : undefined
      const name = agentName(agent)
      const message = str(input?.message)
      return {
        icon: Bot,
        title: item.name === SEND ? `Message ${name}` : `Stop ${name}`,
        meta: item.name === SEND ? oneLine(message) : undefined,
        description: message ? <Markdown text={message} softBreaks /> : undefined,
        children: id ? <OpenAgent id={id} /> : undefined,
      }
    }
    case LIST:
      return { icon: Bot, title: "Check on agents" }
    default:
      return null
  }
}

/** How a report says its run ended, as its first line has it. */
function endedOf(firstLine: string): { label: string; tone?: "bad" } | null {
  if (firstLine.startsWith("The agent finished")) return { label: "Finished" }
  if (firstLine.startsWith("The agent was stopped")) return { label: "Stopped" }
  if (firstLine.startsWith("The agent was interrupted by a restart")) return { label: "Interrupted" }
  if (firstLine.startsWith("The agent failed")) return { label: "Failed", tone: "bad" }
  return null
}

/**
 * A report splits into how the run ended and what the agent said. The host
 * writes the first as one line, then "Its report:" and the agent's words.
 */
export function splitReport(text: string): {
  ended: ReturnType<typeof endedOf>
  detail: string | null
  said: string
} {
  const [first = "", ...rest] = text.split("\n")
  const ended = endedOf(first)
  if (!ended) return { ended: null, detail: null, said: text }
  const body = rest.join("\n")
  const said = body.startsWith("Its report:") ? body.slice("Its report:".length).trim() : ""
  // A failure says what went wrong after the colon; anything else has nothing to add.
  const detail = ended.tone === "bad" ? first.replace(/^The agent failed:\s*/, "") || null : null
  return { ended, detail, said }
}

/**
 * A message another agent sent this session: a child's report, under that
 * agent's name, with how its run ended and a way into its thread. On the
 * agent's side of the thread, not the people's: no person wrote it.
 */
export function AgentMessage({ input, text }: { input: TurnInput; text: string }) {
  const agents = React.useContext(SessionAgentsContext)
  const id = input.author?.id as Uuid | undefined
  const agent = id ? agents.get(id) : undefined
  const name = agent ? agentName(agent) : input.author?.name?.trim() || "An agent"
  const { ended, detail, said } = splitReport(text)

  return (
    <div data-thread-block data-agent-message className="mr-auto flex max-w-[85%] flex-col gap-1.5">
      <div className="flex max-w-full items-center gap-1.5 pl-1 text-xs text-muted-foreground">
        <span className="flex size-5 items-center justify-center rounded-full border">
          <Bot className="size-3" />
        </span>
        <span className="truncate font-medium text-foreground">{name}</span>
        {ended ? (
          <span
            className={cn(
              "shrink-0 rounded-full border px-1.5 py-px text-[10px] leading-4",
              ended.tone === "bad" && "border-destructive/40 text-destructive",
            )}
          >
            {ended.label}
          </span>
        ) : null}
        {id ? (
          <Link
            to="/session/$sessionId"
            params={{ sessionId: id }}
            aria-label={`Open ${name}'s thread`}
            className="shrink-0 hover:text-foreground"
          >
            <ExternalLink className="size-3" />
          </Link>
        ) : null}
      </div>
      <div className="rounded-2xl border border-dashed bg-background px-4 py-2.5 text-[15px] leading-relaxed text-foreground">
        {detail ? <p className="mb-1 text-sm text-destructive">{detail}</p> : null}
        {said ? (
          <Markdown text={said} softBreaks />
        ) : (
          <p className="text-sm text-muted-foreground">{ended ? "It said nothing." : text}</p>
        )}
      </div>
    </div>
  )
}
