/** How a session is named and keyed by the app frame — sidebar and drawer alike. */

import type { Session, Uuid } from "@/lib/api"

/** The one active-item key a session row claims, shared across every nav surface. */
export function sessionNavKey(id: Uuid): string {
  return `session:${id}`
}

/** A session's title, or the placeholder shown until the model writes one. */
export function sessionLabel(session: Session): string {
  return session.title?.trim() || "Untitled thread"
}

/** The active-item key a project's own page claims. */
export function projectNavKey(id: Uuid): string {
  return `project:${id}`
}

const RELATIVE = new Intl.RelativeTimeFormat("en", { numeric: "auto" })
const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 365 * 24 * 3600],
  ["month", 30 * 24 * 3600],
  ["week", 7 * 24 * 3600],
  ["day", 24 * 3600],
  ["hour", 3600],
  ["minute", 60],
]

/** "2 hours ago", "yesterday", "just now" — for lists of threads. */
export function timeAgo(epochSeconds: number, now: number = Date.now()): string {
  const elapsed = Math.max(0, now / 1000 - epochSeconds)
  for (const [unit, seconds] of UNITS) {
    if (elapsed >= seconds) return RELATIVE.format(-Math.floor(elapsed / seconds), unit)
  }
  return "just now"
}
