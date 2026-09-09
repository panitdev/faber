"use client"

import * as React from "react"
import { AnimatePresence, motion, useReducedMotion } from "framer-motion"

import { FaberMark, MARK_ASPECT } from "@/components/thread/faber-mark"
import { PANIT_DEFAULT_EASE } from "@/lib/motion"
import { cn } from "@/lib/utils"

/** Verbs shown while a run is in flight, one at a time, in no fixed order. */
const WORKING_TEXTS = [
  "Working",
  "Figuring",
  "Shaping",
  "Refining",
  "Thinking",
  "Considering",
  "Forming",
  "Weaving",
]

const SWAP_INTERVAL_MS = 5000
const MARK_SIZE = 36

function pickText(exclude?: string): string {
  const pool = exclude ? WORKING_TEXTS.filter((text) => text !== exclude) : WORKING_TEXTS
  return pool[Math.floor(Math.random() * pool.length)]
}

/**
 * The Faber presence line, sitting under the last agent run.
 *
 * Idle it is just the mark. While a run is in flight the mark grows wings and a
 * verb rides along beside it, swapping every {@link SWAP_INTERVAL_MS} for
 * another picked at random — the switch is a fade out/in, and the word carries
 * a skeleton-style sweep so the whole line reads as "still going".
 *
 * The mark's box is centred on the agent timeline's node axis. The box is fixed
 * across both states — wide enough for the wings — so nothing shifts when they
 * unfurl.
 */
export function FaberIndicator({
  className,
  nodeSize = 64,
  working = false,
}: {
  className?: string
  /** Node diameter of the run above; the mark centres on its axis. */
  nodeSize?: number
  working?: boolean
}) {
  const reduce = useReducedMotion()
  // Seeded at random rather than from a fixed head of the list, so a run does
  // not always open on the same word. Only ever read while `working`, which is
  // false on the server's first paint — no hydration mismatch.
  const [text, setText] = React.useState(pickText)

  React.useEffect(() => {
    if (!working) return

    const id = setInterval(() => setText((prev) => pickText(prev)), SWAP_INTERVAL_MS)
    return () => clearInterval(id)
  }, [working])

  return (
    <div
      className={cn("flex items-center gap-2", className)}
      style={{ marginLeft: nodeSize / 2 - (MARK_SIZE * MARK_ASPECT) / 2 }}
    >
      <FaberMark size={MARK_SIZE} working={working} />

      <AnimatePresence mode="wait" initial={false}>
        {working ? (
          <motion.span
            key={text}
            className={cn(
              "inline-block text-sm",
              reduce
                ? "text-muted-foreground"
                : // The gradient tile is twice the text width and travels
                // exactly one tile per cycle, so the loop has no seam. The
                // lit zone spans most of the tile so some of it is always
                // over the text — a continuous glow, not a passing flash.
                "animate-text-shimmer bg-clip-text text-transparent [background-image:linear-gradient(100deg,var(--muted-foreground)_10%,var(--foreground)_50%,var(--muted-foreground)_90%)] [background-size:200%_100%]",
            )}
            initial={{ opacity: 0, y: 2 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -2 }}
            transition={reduce ? { duration: 0 } : { duration: 0.24, ease: PANIT_DEFAULT_EASE }}
          >
            {text}
          </motion.span>
        ) : null}
      </AnimatePresence>

      {working ? (
        <span className="relative inline-flex h-1.5 w-1.5 translate-y-px">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-foreground/60 opacity-75" />
          <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-foreground/60" />
        </span>
      ) : null}
    </div>
  )
}
