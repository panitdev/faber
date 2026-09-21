"use client"

import * as React from "react"

import { SquircleFrame } from "@/components/util/squircle-frame"
import { cn } from "@/lib/utils"
import type { UsageSummary } from "@/lib/thread/usage"

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

export type UsageCardProps = {
  summary: UsageSummary
  className?: string
}

/**
 * The thread's running token and cost totals, as a small squircle HUD pinned
 * over the transcript's top-left corner. Read-only: everything it shows is
 * derived from messages already on screen.
 */
export function UsageCard({ summary, className }: UsageCardProps) {
  const cost =
    summary.cost === null
      ? "—"
      : `${summary.approximate ? "≈" : ""}${formatCost(summary.cost)}`

  return (
    <SquircleFrame
      cornerRadius={16}
      borderWidth={1}
      cornerSmoothing={1}
      className={cn("pointer-events-none shadow-[0_10px_30px_rgba(0,0,0,0.05)]", className)}
      faceClassName="bg-background/85 backdrop-blur-sm"
    >
      <div className="flex min-w-[10rem] flex-col gap-1.5 px-3.5 py-3">
        <span className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
          Usage
        </span>
        <div className="flex items-baseline justify-between gap-4">
          <span className="text-xs text-muted-foreground">Tokens</span>
          <span className="font-mono text-sm tabular-nums text-foreground">
            {formatTokens(summary.totalTokens)}
          </span>
        </div>
        <div className="flex items-baseline justify-between gap-4">
          <span className="text-xs text-muted-foreground">Cost</span>
          <span className="font-mono text-sm tabular-nums text-foreground">{cost}</span>
        </div>
        <span className="mt-0.5 text-[11px] tabular-nums text-muted-foreground">
          {formatTokens(summary.inputTokens)} in · {formatTokens(summary.outputTokens)} out
        </span>
      </div>
    </SquircleFrame>
  )
}
