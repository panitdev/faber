"use client"

import * as React from "react"

import {
  faber,
  FaberError,
  type ModelConfig,
  type ThinkingSelection,
  type Uuid,
} from "@/lib/api"

export type SessionSelection = {
  /** The model this session's next message goes to, or `null` if there is none. */
  model: ModelConfig | null
  /** The thinking knob; `null` is the model's own default. */
  thinking: ThinkingSelection | null
  selectModel: (alias: string) => void
  selectThinking: (selection: ThinkingSelection | null) => void
  /** Set when a pick failed to save — the shown value has been put back. */
  error: string | null
}

/**
 * A session's own model and thinking knob, persisted the moment they change.
 *
 * Written on pick rather than on send, because that is when the user believes
 * they made the choice: a selection that only survives if a message follows
 * would come back wrong on the next reload, which is exactly the case someone
 * setting a session up and walking away runs into.
 *
 * `fallback` covers the window before the session's own row arrives, and the
 * sessions that never picked anything — the app-shell's draft, which is what
 * the landing page's pickers write to. It is display only: nothing here writes
 * a fallback back to the session, so a model the user never chose can't end up
 * looking like one they did.
 */
export function useSessionSelection(
  sessionId: Uuid,
  models: ModelConfig[],
  fallback: { model: ModelConfig | null; thinking: ThinkingSelection | null },
): SessionSelection {
  const [stored, setStored] = React.useState<{
    model: string | null
    thinking: ThinkingSelection | null
  } | null>(null)
  const [error, setError] = React.useState<string | null>(null)

  // A pick made before the row arrives is newer than the row: the fetch is
  // in flight for a moment, and landing it afterwards would silently undo a
  // choice that was already saved.
  const picked = React.useRef(false)

  React.useEffect(() => {
    let cancelled = false

    void faber
      .getSession(sessionId)
      .then((session) => {
        if (!cancelled && !picked.current) {
          setStored({ model: session.model, thinking: session.thinking_effort })
        }
      })
      // Deliberately silent: the fallback still names a model to send to, and
      // failing to read the stored pick must not read as failing to send.
      .catch(() => undefined)

    return () => {
      cancelled = true
    }
  }, [sessionId])

  const model =
    (stored?.model ? models.find((candidate) => candidate.alias === stored.model) : null) ??
    fallback.model
  const thinking = stored ? stored.thinking : fallback.thinking

  // Optimistic, then put back on failure. A knob that stays where it was put
  // while the server rejected it is the one outcome worth avoiding — the next
  // message would run at something the user cannot see.
  const persist = React.useCallback(
    (
      next: { model: string | null; thinking: ThinkingSelection | null },
      patch: Parameters<typeof faber.updateSession>[1],
    ) => {
      const previous = stored
      picked.current = true
      setStored(next)
      setError(null)

      void faber.updateSession(sessionId, patch).catch((err) => {
        setStored(previous)
        setError(err instanceof FaberError ? err.message : "failed to save the selection")
      })
    },
    [sessionId, stored],
  )

  const selectModel = React.useCallback(
    (alias: string) => {
      persist({ model: alias, thinking: stored?.thinking ?? thinking }, { model: alias })
    },
    [persist, stored, thinking],
  )

  const selectThinking = React.useCallback(
    (selection: ThinkingSelection | null) => {
      // The model half is left exactly as stored — including "nothing stored",
      // which keeps falling back rather than quietly adopting the fallback as
      // a pick nobody made.
      persist(
        { model: stored?.model ?? null, thinking: selection },
        { thinking_effort: selection },
      )
    },
    [persist, stored],
  )

  return { model, thinking, selectModel, selectThinking, error }
}
