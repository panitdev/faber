import * as React from "react"

import { faber, type SessionAgent, type Uuid } from "@/lib/api"

/** How long to wait before opening the feed again after it drops. */
const RECONNECT_MS = 3_000

/**
 * The agents `sessionId` spawned, kept current from its agents feed: each one
 * by id, as the feed last sent it. Empty until the feed answers, and for a
 * session that spawned none. The feed replays every agent when it opens, so
 * reconnecting after a drop picks up whatever changed meanwhile.
 */
export function useSessionAgents(sessionId: Uuid | null): ReadonlyMap<Uuid, SessionAgent> {
  const [agents, setAgents] = React.useState<ReadonlyMap<Uuid, SessionAgent>>(() => new Map())

  React.useEffect(() => {
    if (!sessionId) return
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout> | undefined

    const open = async () => {
      try {
        for await (const agent of faber.streamAgents(sessionId, controller.signal)) {
          setAgents((current) => new Map(current).set(agent.id, agent))
        }
      } catch {
        // Dropped, or refused; tried again below unless this view is gone.
      }
      if (!controller.signal.aborted) timer = setTimeout(() => void open(), RECONNECT_MS)
    }
    void open()

    return () => {
      controller.abort()
      clearTimeout(timer)
    }
  }, [sessionId])

  return agents
}
