"use client"

import * as React from "react"

import type { ExchangeTiming } from "@/lib/api"

const HEIGHT = 72

/**
 * One call's output rate over time, from dispatch to its last content.
 *
 * The provider streams text and reports tokens only as a total, so the record
 * holds characters per slice. With a token total the slices are scaled to
 * tokens per second, each taking its share of the total by its characters;
 * without one the chart falls back to characters per second and says so.
 */
export function PaceChart({
  timing,
  outputTokens,
}: {
  timing: ExchangeTiming
  outputTokens: number | null
}) {
  const [hover, setHover] = React.useState<number | null>(null)

  const { rates, unit } = React.useMemo(() => {
    const seconds = timing.bucket_ms / 1000
    const total = timing.chars.reduce((sum, count) => sum + count, 0)
    const inTokens = outputTokens !== null && outputTokens > 0 && total > 0
    const scale = inTokens ? outputTokens / total : 1
    return {
      rates: timing.chars.map((count) => (count * scale) / seconds),
      unit: inTokens ? "tok/s" : "chars/s",
    }
  }, [timing, outputTokens])

  if (rates.length < 2) return null

  const peak = Math.max(...rates)
  const count = rates.length
  // A step per slice: each value is a total over its slice, not a reading at
  // one instant, so the line holds flat across it.
  const y = (rate: number) => (peak === 0 ? HEIGHT : HEIGHT - (rate / peak) * (HEIGHT - 2))
  let line = ""
  rates.forEach((rate, index) => {
    line += `${index === 0 ? "M" : "L"}${index},${y(rate)} L${index + 1},${y(rate)} `
  })
  const area = `${line}L${count},${HEIGHT} L0,${HEIGHT} Z`

  const onMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const box = event.currentTarget.getBoundingClientRect()
    if (box.width === 0) return
    const index = Math.floor(((event.clientX - box.left) / box.width) * count)
    setHover(Math.min(count - 1, Math.max(0, index)))
  }

  const endSeconds = (count * timing.bucket_ms) / 1000
  const summary =
    hover === null
      ? `peak ${formatRate(peak)} ${unit}`
      : `${formatSeconds((hover * timing.bucket_ms) / 1000)} · ${formatRate(rates[hover])} ${unit}`

  return (
    <div className="flex min-w-0 flex-col gap-1 px-3">
      <div className="flex items-baseline justify-between text-xs">
        <span className="font-medium text-muted-foreground">Output rate</span>
        <span className="font-mono text-[11px] tabular-nums">{summary}</span>
      </div>
      <div
        role="img"
        aria-label={`Output rate over ${formatSeconds(endSeconds)}, peak ${formatRate(peak)} ${unit}`}
        className="relative touch-none border-b border-border text-foreground"
        style={{ height: HEIGHT }}
        onPointerMove={onMove}
        onPointerLeave={() => setHover(null)}
      >
        <svg
          viewBox={`0 0 ${count} ${HEIGHT}`}
          preserveAspectRatio="none"
          className="absolute inset-0 size-full"
          aria-hidden
        >
          <path d={area} fill="currentColor" fillOpacity={0.08} />
          <path
            d={line}
            fill="none"
            stroke="currentColor"
            strokeWidth={1.5}
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />
        </svg>
        {hover !== null ? (
          <div
            className="pointer-events-none absolute inset-y-0 w-px bg-foreground/40"
            style={{ left: `${((hover + 0.5) / count) * 100}%` }}
          />
        ) : null}
      </div>
      <div className="flex justify-between font-mono text-[10px] text-muted-foreground tabular-nums">
        <span>0s</span>
        <span>{formatSeconds(endSeconds)}</span>
      </div>
    </div>
  )
}

function formatRate(rate: number): string {
  return rate >= 100 ? rate.toFixed(0) : rate.toFixed(1)
}

function formatSeconds(seconds: number): string {
  return `${seconds >= 100 ? seconds.toFixed(0) : seconds.toFixed(1)}s`
}
