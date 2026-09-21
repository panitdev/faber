"use client"

import * as React from "react"
import { AnimatePresence, motion, useReducedMotion } from "framer-motion"

import { SquircleFrame } from "@/components/util/squircle-frame"
import { PANIT_DEFAULT_EASE } from "@/lib/motion"
import { cn } from "@/lib/utils"
import type { UsageSummary } from "@/lib/thread/usage"

const SCOPES = [
  { id: "all", label: "All" },
  { id: "last", label: "Last turn" },
] as const

type Scope = (typeof SCOPES)[number]["id"]

/** The indicator's word-swap timing, reused so the two read as one motion. */
const SLIDE_TRANSITION = {
  duration: 0.32,
  ease: PANIT_DEFAULT_EASE,
} as const

/** Compact token counts: `820`, `12.3k`, `4.20M`. */
function formatTokens(count: number): string {
  if (count < 1_000) return String(count)
  if (count < 1_000_000) {
    const thousands = count / 1_000
    return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`
  }
  return `${(count / 1_000_000).toFixed(2)}M`
}

/** Money at a precision that stays meaningful when a thread costs cents. */
function formatCost(usd: number): string {
  if (usd === 0) return "$0"
  if (usd < 0.01) return `$${usd.toFixed(4)}`
  if (usd < 1) return `$${usd.toFixed(3)}`
  return `$${usd.toFixed(2)}`
}

function rowsFor(summary: UsageSummary): { label: string; value: string }[] {
  return [
    { label: "Input", value: formatTokens(summary.inputTokens) },
    { label: "Output", value: formatTokens(summary.outputTokens) },
    { label: "Cached input", value: formatTokens(summary.cacheReadTokens) },
    { label: "Reasoning", value: formatTokens(summary.reasoningTokens) },
    {
      label: "Cost",
      value:
        summary.cost === null
          ? "—"
          : `${summary.approximate ? "≈" : ""}${formatCost(summary.cost)}`,
    },
  ]
}

/**
 * One field's figure, sliding to its next value the way the Faber indicator
 * swaps its word: the outgoing number rides up and out while the incoming one
 * rises from below. The height is fixed and the overflow clipped, so only the
 * glyphs move.
 */
function SlidingValue({ value }: { value: string }) {
  const reduce = useReducedMotion()

  return (
    <span className="relative inline-flex h-5 items-center justify-end overflow-hidden">
      <AnimatePresence mode="popLayout" initial={false}>
        <motion.span
          key={value}
          className="inline-flex tabular-nums"
          initial={reduce ? false : { opacity: 0, y: "100%", filter: "blur(2px)" }}
          animate={{ opacity: 1, y: 0, filter: "blur(0px)" }}
          exit={{ opacity: 0, y: "-100%", filter: "blur(2px)" }}
          transition={reduce ? { duration: 0 } : SLIDE_TRANSITION}
        >
          {value}
        </motion.span>
      </AnimatePresence>
    </span>
  )
}

export type UsageCardProps = {
  /** Every turn in the session. */
  all: UsageSummary
  /** The most recent turn that reported usage — see the call site. */
  last: UsageSummary
  className?: string
}

/**
 * The session's token and cost figures, as a small squircle HUD pinned over
 * the transcript's top-left corner. Two scopes over the same five rows: the
 * whole session, or the newest turn. Read-only, except for the tab switch.
 */
export function UsageCard({ all, last, className }: UsageCardProps) {
  const [scope, setScope] = React.useState<Scope>("all")
  const tabs = React.useRef<(HTMLButtonElement | null)[]>([])
  // Prefixed per instance so the tablist and panel ids cannot collide if more
  // than one ever renders.
  const id = React.useId()
  const summary = scope === "last" ? last : all

  const select = (next: number) => {
    const wrapped = (next + SCOPES.length) % SCOPES.length
    setScope(SCOPES[wrapped].id)
    tabs.current[wrapped]?.focus()
  }

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return
    event.preventDefault()
    const index = SCOPES.findIndex((option) => option.id === scope)
    select(event.key === "ArrowRight" ? index + 1 : index - 1)
  }

  return (
    <SquircleFrame
      cornerRadius={16}
      borderWidth={1}
      cornerSmoothing={1}
      className={cn("shadow-[0_10px_30px_rgba(0,0,0,0.05)]", className)}
      faceClassName="bg-background/85 backdrop-blur-sm"
    >
      <div className="flex min-w-[12.5rem] flex-col gap-2.5 px-3.5 py-3">
        <div
          role="tablist"
          aria-label="Token usage scope"
          className="flex items-center gap-0.5 rounded-full bg-muted p-0.5"
        >
          {SCOPES.map((option, index) => {
            const selected = option.id === scope
            return (
              <button
                key={option.id}
                ref={(node) => {
                  tabs.current[index] = node
                }}
                type="button"
                role="tab"
                id={`${id}-tab-${option.id}`}
                aria-selected={selected}
                aria-controls={`${id}-panel`}
                tabIndex={selected ? 0 : -1}
                onClick={() => setScope(option.id)}
                onKeyDown={onKeyDown}
                className={cn(
                  "flex-1 rounded-full px-2.5 py-1 text-[11px] font-medium whitespace-nowrap transition-colors",
                  selected
                    ? "bg-background text-foreground shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {option.label}
              </button>
            )
          })}
        </div>

        <dl
          id={`${id}-panel`}
          role="tabpanel"
          aria-labelledby={`${id}-tab-${scope}`}
          className="flex flex-col gap-1"
        >
          {rowsFor(summary).map((row) => (
            <div key={row.label} className="flex items-center justify-between gap-4">
              <dt className="text-xs text-muted-foreground">{row.label}</dt>
              <dd className="font-mono text-sm text-foreground">
                <SlidingValue value={row.value} />
              </dd>
            </div>
          ))}
        </dl>
      </div>
    </SquircleFrame>
  )
}
