"use client"

import * as React from "react"

import type { AgentThinkingMode } from "@charlotte-ui/labs/agent-run"

/**
 * How much of each thinking block is showing, and what a click on one does.
 *
 * The block's own default (`collapsed` ↔ `expanded`) is not what a run wants:
 * reasoning that is still arriving should peek, and reasoning that has stopped
 * should get out of the way. So mode is derived from two things — whether the
 * block is still streaming, which the transcript knows, and whether the user
 * has opened it, which is all this hook stores:
 *
 * | | not opened | opened |
 * | --- | --- | --- |
 * | streaming | `peek` | `expanded` |
 * | ended | `collapsed` | `expanded` |
 *
 * A block the user opened mid-stream therefore stays open when it ends, while
 * one left peeking folds away on its own. Closing a block is forgetting the
 * override rather than recording one, which is what makes both toggles the
 * spec asks for — `peek` ↔ `expanded` while streaming, `collapsed` ↔
 * `expanded` after — the same single piece of state.
 */
export type ThinkingModes = {
  /** The mode to render a block in, given whether it is still streaming. */
  modeOf: (id: string, streaming: boolean) => AgentThinkingMode
  /** Handles a header click on a block currently rendered in `mode`. */
  toggle: (id: string, mode: AgentThinkingMode) => void
}

export function useThinkingModes(): ThinkingModes {
  const [opened, setOpened] = React.useState<Record<string, true>>({})

  const modeOf = React.useCallback(
    (id: string, streaming: boolean): AgentThinkingMode => {
      if (opened[id]) return "expanded"
      return streaming ? "peek" : "collapsed"
    },
    [opened],
  )

  const toggle = React.useCallback((id: string, mode: AgentThinkingMode) => {
    setOpened((current) => {
      if (mode !== "expanded") return { ...current, [id]: true }
      const next = { ...current }
      delete next[id]
      return next
    })
  }, [])

  return { modeOf, toggle }
}
