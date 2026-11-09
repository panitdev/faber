import * as React from "react"
import { createFileRoute, useNavigate } from "@tanstack/react-router"
import { MessageSquare, Plus } from "lucide-react"

import type { Session } from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import {
  DeleteSessionDialog,
  DeleteSessionsDialog,
  RenameSessionDialog,
} from "@/components/shell/session-dialogs"
import { ThreadList } from "@/components/thread/thread-list"
import { Button } from "@charlotte-ui/button"

export const Route = createFileRoute("/threads")({ component: ThreadsPage })

/**
 * The user's global threads — the ones that belong to no project.
 *
 * Project threads stay on their project's page, where the plugins and context
 * they run with are stated. This page is the workspace-wide view of everything
 * started outside a project.
 */
function ThreadsPage() {
  const navigate = useNavigate()
  const {
    sessions,
    sessionsLoading,
    createError,
    createSession,
    creatingSession,
    renameSession,
    deleteSession,
  } = useAppShell()

  const [renameTarget, setRenameTarget] = React.useState<Session | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Session | null>(null)
  const [deleteManyTarget, setDeleteManyTarget] = React.useState<Session[] | null>(null)
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
  const threads = sessions.filter((session) => session.project_id === null)

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-6 px-4 py-6 md:py-10">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Threads</h1>
            <p className="text-sm text-muted-foreground">
              Threads outside any project, newest first.
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

        <ThreadList
          threads={threads}
          loading={sessionsLoading}
          onRename={openRename}
          onDelete={setDeleteTarget}
          onDeleteSelected={setDeleteManyTarget}
          empty={
            <div className="flex flex-col items-center gap-3 py-4 text-center">
              <MessageSquare className="size-8 text-muted-foreground" />
              <div className="flex flex-col gap-1">
                <p className="text-sm font-medium">No threads yet</p>
                <p className="text-sm text-muted-foreground">
                  Start one here, or write on the home page.
                </p>
              </div>
              <Button size="sm" onClick={() => void startThread()} disabled={creatingSession}>
                <Plus className="h-4 w-4" />
                New thread
              </Button>
            </div>
          }
        />
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

      <DeleteSessionsDialog
        sessions={deleteManyTarget}
        onOpenChange={(open) => !open && setDeleteManyTarget(null)}
        onDelete={deleteSession}
      />
    </div>
  )
}
