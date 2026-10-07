/**
 * Turns a primitive's validated `style.*` properties into inline styles.
 *
 * Every input has already passed `widgets.ts`, so each value is an enumerated
 * word, a bounded number or a theme token; this file only maps them, and
 * never forwards a plugin string into CSS as-is.
 */

import type * as React from "react"

import { COLOR_TOKENS, type Json } from "./widgets"

type Props = Record<string, Json>

const STEP_REM = 0.25

function spacing(value: Json): string | undefined {
  if (typeof value === "number") return `${value * STEP_REM}rem`
  if (Array.isArray(value)) return value.map((part) => `${(part as number) * STEP_REM}rem`).join(" ")
  return undefined
}

function length(value: Json): string | undefined {
  if (typeof value === "number") return `${value}px`
  if (value === "full") return "100%"
  if (value === "fit") return "fit-content"
  if (value === "auto") return "auto"
  if (typeof value === "string" && value.endsWith("%")) return value
  return undefined
}

function token(value: Json): string | undefined {
  return typeof value === "string" && Object.hasOwn(COLOR_TOKENS, value)
    ? COLOR_TOKENS[value as keyof typeof COLOR_TOKENS]
    : undefined
}

const FLEX_ALIGN: Record<string, string> = {
  start: "flex-start",
  center: "center",
  end: "flex-end",
  stretch: "stretch",
  baseline: "baseline",
}

const JUSTIFY: Record<string, string> = {
  start: "flex-start",
  center: "center",
  end: "flex-end",
  between: "space-between",
  around: "space-around",
  evenly: "space-evenly",
}

const RADIUS: Record<string, string> = {
  none: "0",
  sm: "var(--radius-sm)",
  md: "var(--radius-md)",
  lg: "var(--radius-lg)",
  xl: "var(--radius-xl)",
  full: "9999px",
}

const SHADOW: Record<string, string> = {
  none: "none",
  sm: "0 1px 2px 0 rgb(0 0 0 / 0.06)",
  md: "0 4px 12px -2px rgb(0 0 0 / 0.1)",
  lg: "0 12px 32px -8px rgb(0 0 0 / 0.18)",
}

const TEXT_SIZE: Record<string, string> = {
  xs: "0.75rem",
  sm: "0.875rem",
  base: "1rem",
  lg: "1.125rem",
  xl: "1.25rem",
  "2xl": "1.5rem",
}

const WEIGHT: Record<string, number> = { normal: 400, medium: 500, semibold: 600, bold: 700 }

function pick<T>(table: Record<string, T>, value: Json): T | undefined {
  return typeof value === "string" && Object.hasOwn(table, value) ? table[value] : undefined
}

/** Sizing, spacing and decoration, shared by every primitive and by `scroll`. */
function common(props: Props): React.CSSProperties {
  return {
    flexGrow: props["style.grow"] as number | undefined,
    flexBasis: length(props["style.basis"]),
    width: length(props["style.width"]),
    height: length(props["style.height"]),
    minWidth: length(props["style.min_width"]),
    maxWidth: length(props["style.max_width"]),
    minHeight: length(props["style.min_height"]),
    maxHeight: length(props["style.max_height"]),
    padding: spacing(props["style.padding"]),
    margin: spacing(props["style.margin"]),
    overflow: pick({ visible: "visible", hidden: "hidden", auto: "auto" }, props["style.overflow"]),
    background: token(props["style.bg"]),
    color: token(props["style.fg"]),
    border: token(props["style.border"]) && `1px solid ${token(props["style.border"])}`,
    borderRadius: pick(RADIUS, props["style.radius"]),
    boxShadow: pick(SHADOW, props["style.shadow"]),
    opacity: props["style.opacity"] as number | undefined,
  }
}

export function boxStyle(props: Props): React.CSSProperties {
  const grid = props["style.display"] === "grid"
  const columns = props["style.columns"]
  return {
    display: grid ? "grid" : "flex",
    flexDirection: grid ? undefined : props["style.direction"] === "row" ? "row" : "column",
    gridTemplateColumns:
      grid && typeof columns === "number" ? `repeat(${columns}, minmax(0, 1fr))` : undefined,
    flexWrap: props["style.wrap"] === true ? "wrap" : undefined,
    gap: spacing(props["style.gap"]),
    alignItems: pick(FLEX_ALIGN, props["style.align"]),
    justifyContent: pick(JUSTIFY, props["style.justify"]),
    minWidth: 0,
    ...strip(common(props)),
  }
}

export function textStyle(props: Props): React.CSSProperties {
  const truncate = props["style.truncate"] === true
  return {
    display: "block",
    fontSize: pick(TEXT_SIZE, props["style.size"]),
    fontWeight: pick(WEIGHT, props["style.weight"]),
    fontFamily: props["style.mono"] === true ? "var(--font-mono)" : undefined,
    fontStyle: props["style.italic"] === true ? "italic" : undefined,
    textAlign: pick({ start: "start", center: "center", end: "end" } as const, props["style.align"]),
    overflow: truncate ? "hidden" : undefined,
    textOverflow: truncate ? "ellipsis" : undefined,
    whiteSpace: truncate ? "nowrap" : "pre-wrap",
    overflowWrap: "anywhere",
    ...strip(common(props)),
  }
}

export function scrollStyle(props: Props): React.CSSProperties {
  return strip({
    height: length(props["style.height"]),
    maxHeight: length(props["style.max_height"]),
  })
}

/** Drops unset keys, so they do not override what a spread before them set. */
function strip(style: React.CSSProperties): React.CSSProperties {
  return Object.fromEntries(
    Object.entries(style).filter(([, value]) => value !== undefined),
  ) as React.CSSProperties
}
