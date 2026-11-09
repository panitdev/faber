import * as React from "react"
import { Link } from "@tanstack/react-router"
import { Pencil, Trash2 } from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { sessionLabel, timeAgo } from "@/lib/sessions/labels"
import { cn } from "@charlotte-ui/utils"
import { Button } from "@charlotte-ui/button"
import { Checkbox } from "@/components/ui/checkbox"

/**
 * Threads as the project page's "Recents" list, shared with the Threads page.
 *
 * A row is one line: the title, where it runs, and when it started. The whole
 * row opens the thread; the project name is its own link. Actions are passed a
 * target rather than owned here, so each page keeps its dialogs.
 *
 * Selection is the list's own state: a deleted row drops out of it by no longer
 * being in `threads`, so a page only has to act on what it is handed.
 */
export function ThreadList({
  threads,
  className,
  loading = false,
  empty,
  projectName,
  onRename,
  onDelete,
  onDeleteSelected,
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
  /** When given, rows can be selected and deleted together. */
  onDeleteSelected?: (sessions: Session[]) => void
}) {
  const [selectedIds, setSelectedIds] = React.useState<ReadonlySet<Uuid>>(() => new Set())
  const selected = threads.filter((session) => selectedIds.has(session.id))

  const setRowSelected = (id: Uuid, isSelected: boolean) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (isSelected) next.add(id)
      else next.delete(id)
      return next
    })
  }

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
    <div className={className}>
      <ul>
      {threads.map((session) => {
        const label = sessionLabel(session)
        const hasActions = !!onRename || !!onDelete
        const isSelected = selectedIds.has(session.id)
        return (
          <li key={session.id} className="border-t border-border/60">
            {/* Keyed to keyboard focus: a clicked checkbox keeps focus, and
                must not leave the row looking hovered once the pointer goes. */}
            <div
              className={cn(
                "group/thread relative -mx-2 flex items-center gap-4 rounded-md px-2 py-3.5 transition-colors hover:bg-accent/60 has-[:focus-visible]:bg-accent/60",
                isSelected && "bg-accent/40",
              )}
            >
              {/* Under the row's content, so the whole line opens the thread
                  without swallowing the project link or the actions. */}
              <Link
                to="/session/$sessionId"
                params={{ sessionId: session.id }}
                className="absolute inset-0 z-0 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
              >
                <span className="sr-only">Open {label}</span>
              </Link>

              {onDeleteSelected ? (
                <Checkbox
                  checked={isSelected}
                  onCheckedChange={(checked) => setRowSelected(session.id, checked === true)}
                  aria-label={`Select ${label}`}
                  className={cn(
                    "z-10 data-[state=checked]:border-primary data-[state=checked]:bg-primary data-[state=checked]:text-primary-foreground",
                    // Once anything is selected every row shows its box, so
                    // adding to the selection doesn't mean hunting for it.
                    selected.length === 0 &&
                      "transition-opacity focus-visible:opacity-100 group-hover/thread:opacity-100 [@media(hover:hover)]:opacity-0",
                  )}
                />
              ) : null}

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
                    "group-hover/thread:opacity-0 group-has-[:focus-visible]/thread:opacity-0 [@media(hover:none)]:opacity-0",
                )}
              >
                {timeAgo(session.created_at)}
              </span>

              {hasActions ? (
                <div className="absolute inset-y-0 right-0 z-10 flex items-center justify-end gap-1 transition-opacity [@media(hover:hover)]:pointer-events-none [@media(hover:hover)]:opacity-0 group-hover/thread:pointer-events-auto group-hover/thread:opacity-100 group-has-[:focus-visible]/thread:pointer-events-auto group-has-[:focus-visible]/thread:opacity-100">
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

      {/* After the list rather than above it, so appearing moves no row out
          from under the pointer. */}
      {onDeleteSelected && selected.length > 0 ? (
        <div
          role="toolbar"
          aria-label="Selected threads"
          className="sticky bottom-4 z-20 mt-3 flex items-center gap-2 rounded-lg border border-border bg-popover py-2 pr-2 pl-4 text-popover-foreground shadow-md"
        >
          <span aria-live="polite" className="min-w-0 flex-1 truncate text-sm">
            {selected.length} selected
          </span>
          {selected.length < threads.length ? (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => setSelectedIds(new Set(threads.map((session) => session.id)))}
            >
              Select all
            </Button>
          ) : null}
          <Button size="sm" variant="ghost" onClick={() => setSelectedIds(new Set())}>
            Clear
          </Button>
          <Button size="sm" variant="destructive" onClick={() => onDeleteSelected(selected)}>
            <Trash2 className="h-4 w-4" />
            Delete
          </Button>
        </div>
      ) : null}
    </div>
  )
}
