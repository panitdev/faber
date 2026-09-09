"use client"

import * as React from "react"

import type { Session, Uuid } from "@/lib/api"
import { FaberError } from "@/lib/api"
import { sessionLabel } from "@/lib/sessions/labels"
import { Button } from "@/components/ui/button"
import { AnimatedField } from "@/components/ui/animated-field"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

/**
 * Rename and delete, as dialogs a nav surface drives by setting a target.
 *
 * They live here rather than in the sidebar because the sidebar is only one of
 * two ways to reach them: below `md` it is replaced by the command drawer, and
 * both surfaces open the same two dialogs. Each keeps its own target state —
 * only one of the two is ever on screen, so there is nothing to coordinate.
 *
 * Mount them keyed on the target's id so the form starts from that session's
 * title rather than from whichever one was renamed last.
 */

export function RenameSessionDialog({
  session,
  onOpenChange,
  onRename,
}: {
  session: Session | null
  onOpenChange: (open: boolean) => void
  onRename: (id: Uuid, title: string) => Promise<Session>
}) {
  const [title, setTitle] = React.useState(session?.title ?? "")
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!session) return
    setSubmitting(true)
    setError(null)
    try {
      await onRename(session.id, title)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to rename the thread")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={!!session} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>Rename thread</DialogTitle>
          </DialogHeader>

          <AnimatedField
            id="session-title"
            label="Title"
            value={title}
            onChange={setTitle}
            placeholder="Untitled thread"
            autoFocus
          />

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="submit" loading={submitting} loadingText="Saving">
              Save
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

export function DeleteSessionDialog({
  session,
  onOpenChange,
  onDelete,
}: {
  session: Session | null
  onOpenChange: (open: boolean) => void
  onDelete: (id: Uuid) => Promise<void>
}) {
  return (
    <AlertDialog open={!!session} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            Delete {session ? sessionLabel(session) : "thread"}?
          </AlertDialogTitle>
          <AlertDialogDescription>
            This deletes every thread, run, and message in it. This can&apos;t be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <DeleteSessionFooter
          session={session}
          onDelete={onDelete}
          onDone={() => onOpenChange(false)}
        />
      </AlertDialogContent>
    </AlertDialog>
  )
}

function DeleteSessionFooter({
  session,
  onDelete,
  onDone,
}: {
  session: Session | null
  onDelete: (id: Uuid) => Promise<void>
  onDone: () => void
}) {
  const [deleting, setDeleting] = React.useState(false)

  const handleDelete = async () => {
    if (!session) return
    setDeleting(true)
    try {
      await onDelete(session.id)
      onDone()
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeleting(false)
    }
  }

  return (
    <AlertDialogFooter>
      <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
      <AlertDialogAction
        onClick={(event) => {
          event.preventDefault()
          void handleDelete()
        }}
        disabled={deleting}
        className="bg-destructive text-white hover:bg-destructive/90"
      >
        {deleting ? "Deleting…" : "Delete"}
      </AlertDialogAction>
    </AlertDialogFooter>
  )
}
