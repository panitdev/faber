import * as React from "react"
import { Link } from "@tanstack/react-router"
import { Pencil, Trash2 } from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { sessionLabel, timeAgo } from "@/lib/sessions/labels"
import { cn } from "@charlotte-ui/utils"
import { Button } from "@charlotte-ui/button"

/**
 * Threads as the project page's "Recents" list, shared with the Threads page.
 *
 * A row is one line: the title, where it runs, and when it started. The whole
 * row opens the thread; the project name is its own link. Actions are passed a
 * target rather than owned here, so each page keeps its dialogs.
 */
export function ThreadList({
  threads,
  className,
  loading = false,
  empty,
  projectName,
  onRename,
  onDelete,
}: {
  threads: Session[]
  className?: string
  loading?: boolean
  empty?: React.ReactNode
  /** When given, each row names its project (or "Workspace"). */
  projectName?: (id: Uuid) => string
  /** When given, each row offers rename and delete. */
  onRename?: (session: Session) => void
  onDelete?: (session: Session) => void
}) {
  if (loading && threads.length === 0) {
    return (
      <ul aria-busy="true" aria-label="Loading threads" className={className}>
        {[0, 1, 2].map((row) => (
          <li key={row} className="border-t border-border/60 py-3.5">
            <div className="h-5 w-1/2 animate-pulse rounded bg-muted" />
          </li>
        ))}
      </ul>
    )
  }

  if (threads.length === 0) {
    return (
      <div className={cn("border-t border-border/60 py-4", className)}>
        {empty ?? <p className="text-sm text-muted-foreground">No threads yet.</p>}
      </div>
    )
  }

  return (
    <ul className={className}>
      {threads.map((session) => {
        const label = sessionLabel(session)
        const hasActions = !!onRename || !!onDelete
        return (
          <li key={session.id} className="border-t border-border/60">
            <div className="group/thread relative -mx-2 flex items-center gap-4 rounded-md px-2 py-3.5 transition-colors hover:bg-accent/60 focus-within:bg-accent/60">
              {/* Under the row's content, so the whole line opens the thread
                  without swallowing the project link or the actions. */}
              <Link
                to="/session/$sessionId"
                params={{ sessionId: session.id }}
                className="absolute inset-0 z-0 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
              >
                <span className="sr-only">Open {label}</span>
              </Link>

              <span className="min-w-0 flex-1 truncate text-[15px]">{label}</span>
              {session.closed_at ? (
                <span className="shrink-0 text-xs text-muted-foreground">Closed</span>
              ) : null}

              {projectName ? (
                session.project_id ? (
                  <Link
                    to="/project/$projectId"
                    params={{ projectId: session.project_id }}
                    className="relative z-10 hidden max-w-40 shrink-0 truncate rounded-md text-sm text-muted-foreground outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring/50 sm:block"
                  >
                    {projectName(session.project_id)}
                  </Link>
                ) : (
                  <span className="relative z-10 hidden shrink-0 text-sm text-muted-foreground sm:block">
                    Workspace
                  </span>
                )
              ) : null}

              <span
                className={cn(
                  "shrink-0 text-sm text-muted-foreground transition-opacity",
                  // The actions take the time's place rather than adding a column.
                  hasActions &&
                    "group-hover/thread:opacity-0 group-focus-within/thread:opacity-0 [@media(hover:none)]:opacity-0",
                )}
              >
                {timeAgo(session.created_at)}
              </span>

              {hasActions ? (
                <div className="absolute inset-y-0 right-0 z-10 flex items-center justify-end gap-1 transition-opacity [@media(hover:hover)]:pointer-events-none [@media(hover:hover)]:opacity-0 group-hover/thread:pointer-events-auto group-hover/thread:opacity-100 group-focus-within/thread:pointer-events-auto group-focus-within/thread:opacity-100">
                  {onRename ? (
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Rename ${label}`}
                      onClick={() => onRename(session)}
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>
                  ) : null}
                  {onDelete ? (
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Delete ${label}`}
                      onClick={() => onDelete(session)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  ) : null}
                </div>
              ) : null}
            </div>
          </li>
        )
      })}
    </ul>
  )
}
