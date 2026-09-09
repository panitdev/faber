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
