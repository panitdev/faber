import * as React from "react"
import { createFileRoute, useNavigate } from "@tanstack/react-router"
import { MessageSquare, Pencil, Plus, Trash2 } from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { sessionLabel, timeAgo } from "@/lib/sessions/labels"
import { useAppShell } from "@/components/shell/app-shell"
import {
  DeleteSessionDialog,
  RenameSessionDialog,
} from "@/components/shell/session-dialogs"
import { Button } from "@charlotte-ui/button"
import {
  Table,
  TableBody,
  TableCell,
  TableEmpty,
  TableHead,
  TableHeader,
  TableLoading,
  TableRow,
  TableRowActions,
} from "@charlotte-ui/table"

export const Route = createFileRoute("/threads")({ component: ThreadsPage })

/**
 * Every thread the user has, in one list.
 *
 * The sidebar already answers "what have I been working on" one row at a time;
 * this page is the wider view — threads across projects and the workspace, with
 * the project each one belongs to stated rather than implied by where it sits.
 */
function ThreadsPage() {
  const navigate = useNavigate()
  const {
    sessions,
    sessionsLoading,
    projects,
    createError,
    createSession,
    creatingSession,
    renameSession,
    deleteSession,
  } = useAppShell()

  const [renameTarget, setRenameTarget] = React.useState<Session | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Session | null>(null)
  // Bumped only when the dialog opens, so closing does not remount it.
  const [renameFormKey, setRenameFormKey] = React.useState(0)

  const openRename = (session: Session) => {
    setRenameFormKey((key) => key + 1)
    setRenameTarget(session)
  }

  const startThread = async () => {
    const created = await createSession()
    if (created) navigate({ to: "/session/$sessionId", params: { sessionId: created.id } })
  }

  // The API lists newest first, and a locally created session is prepended, so
  // the order holds without re-sorting. Keeping the copy stable also keeps
  // React from replacing rows while one is being renamed.
  const threads = sessions

  const projectName = React.useCallback(
    (id: Uuid) => projects.find((project) => project.id === id)?.name ?? "Project",
    [projects],
  )

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-6 px-4 py-6 md:py-10">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Threads</h1>
            <p className="text-sm text-muted-foreground">
              Every thread you have started, newest first.
            </p>
          </div>
          <Button
            size="sm"
            className="w-full sm:w-auto"
            loading={creatingSession}
            loadingText="Starting"
            onClick={() => void startThread()}
          >
            <Plus className="h-4 w-4" />
            New thread
          </Button>
        </div>

        {createError ? <p className="text-sm text-destructive">{createError}</p> : null}

        <Table className="table-fixed">
          <TableHeader>
            <TableRow>
              <TableHead>Thread</TableHead>
              <TableHead className="hidden w-44 sm:table-cell">Project</TableHead>
              <TableHead className="hidden w-28 md:table-cell">Started</TableHead>
              <TableHead className="w-px">
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {sessionsLoading ? (
              <TableLoading columns={4} />
            ) : threads.length === 0 ? (
              <TableEmpty
                colSpan={4}
                icon={<MessageSquare />}
                title="No threads yet"
                description="Start one here, or write on the home page or a project's page."
                action={
                  <Button size="sm" onClick={() => void startThread()} disabled={creatingSession}>
                    <Plus className="h-4 w-4" />
                    New thread
                  </Button>
                }
              />
            ) : (
              threads.map((session) => {
                const label = sessionLabel(session)
                return (
                  <TableRow key={session.id}>
                    <TableCell className="py-2">
                      <button
                        type="button"
                        onClick={() =>
                          navigate({
                            to: "/session/$sessionId",
                            params: { sessionId: session.id },
                          })
                        }
                        className="-mx-1 block max-w-full truncate rounded-md px-1 py-0.5 text-left font-medium outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring/50"
                      >
                        {label}
                      </button>
                      {session.closed_at ? (
                        <span className="text-xs text-muted-foreground">Closed</span>
                      ) : null}
                    </TableCell>
                    <TableCell className="hidden py-2 sm:table-cell">
                      {session.project_id ? (
                        <button
                          type="button"
                          onClick={() =>
                            navigate({
                              to: "/project/$projectId",
                              params: { projectId: session.project_id as Uuid },
                            })
                          }
                          className="-mx-1 block max-w-full truncate rounded-md px-1 py-0.5 text-left text-muted-foreground outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring/50"
                        >
                          {projectName(session.project_id)}
                        </button>
                      ) : (
                        <span className="text-muted-foreground">Workspace</span>
                      )}
                    </TableCell>
                    <TableCell className="hidden text-muted-foreground md:table-cell">
                      {timeAgo(session.created_at)}
                    </TableCell>
                    <TableCell align="end" className="w-px">
                      <TableRowActions>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          aria-label={`Rename ${label}`}
                          onClick={() => openRename(session)}
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          aria-label={`Delete ${label}`}
                          onClick={() => setDeleteTarget(session)}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </TableRowActions>
                    </TableCell>
                  </TableRow>
                )
              })
            )}
          </TableBody>
        </Table>
      </div>

      <RenameSessionDialog
        key={renameFormKey}
        session={renameTarget}
        onOpenChange={(open) => !open && setRenameTarget(null)}
        onRename={renameSession}
      />

      <DeleteSessionDialog
        session={deleteTarget}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
        onDelete={deleteSession}
      />
    </div>
  )
}
