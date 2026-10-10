"use client"

import * as React from "react"

import type { Project, Uuid } from "@/lib/api"
import { FaberError } from "@/lib/api"
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
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

/**
 * Create, rename and delete for projects, driven by the nav surfaces the same
 * way `session-dialogs.tsx` is: each surface sets a target, the dialog does
 * the rest. Mount the form keyed on the target so it starts from that
 * project's name.
 */

export function CreateProjectDialog({
  open,
  onOpenChange,
  onCreate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreate: (name: string, description?: string) => Promise<Project>
}) {
  const [name, setName] = React.useState("")
  const [description, setDescription] = React.useState("")
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!name.trim()) {
      setError("A project needs a name.")
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      await onCreate(name, description.trim() || undefined)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to create the project")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>New project</DialogTitle>
            <DialogDescription>
              Threads started in a project share its plugins: the environments they can
              reach and the tools they get.
            </DialogDescription>
          </DialogHeader>

          <AnimatedField
            id="project-name"
            label="Name"
            value={name}
            onChange={setName}
            autoFocus
          />
          <AnimatedField
            id="project-description"
            label="Description (optional)"
            value={description}
            onChange={setDescription}
          />

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="submit" loading={submitting} loadingText="Creating">
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

export function RenameProjectDialog({
  project,
  onOpenChange,
  onRename,
}: {
  project: Project | null
  onOpenChange: (open: boolean) => void
  onRename: (id: Uuid, name: string) => Promise<Project>
}) {
  const [name, setName] = React.useState(project?.name ?? "")
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!project) return
    if (!name.trim()) {
      setError("A project needs a name.")
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      await onRename(project.id, name)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to rename the project")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={!!project} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>Rename project</DialogTitle>
          </DialogHeader>

          <AnimatedField id="project-rename" label="Name" value={name} onChange={setName} autoFocus />

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

export function DeleteProjectDialog({
  project,
  threadCount,
  onOpenChange,
  onDelete,
}: {
  project: Project | null
  /** Threads that go with it, so the cost is stated before it is paid. */
  threadCount: number
  onOpenChange: (open: boolean) => void
  onDelete: (id: Uuid) => Promise<void>
}) {
  const [deleting, setDeleting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleDelete = async () => {
    if (!project) return
    setDeleting(true)
    setError(null)
    try {
      await onDelete(project.id)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to delete the project")
    } finally {
      setDeleting(false)
    }
  }

  return (
    <AlertDialog open={!!project} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete {project?.name ?? "project"}?</AlertDialogTitle>
          <AlertDialogDescription>
            {threadCount === 0
              ? "This deletes the project and its plugin settings."
              : `This deletes the project, its plugin settings, and ${
                  threadCount === 1 ? "its thread" : `all ${threadCount} of its threads`
                } with every message in them.`}{" "}
            This can&apos;t be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        {error ? <p className="text-sm text-destructive">{error}</p> : null}
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
      </AlertDialogContent>
    </AlertDialog>
  )
}

/** Name and description together, for the project's own page. */
export function EditProjectDialog({
  project,
  onOpenChange,
  onSave,
}: {
  project: Project | null
  onOpenChange: (open: boolean) => void
  onSave: (id: Uuid, patch: { name: string; description: string }) => Promise<Project>
}) {
  const [name, setName] = React.useState(project?.name ?? "")
  const [description, setDescription] = React.useState(project?.description ?? "")
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!project) return
    if (!name.trim()) {
      setError("A project needs a name.")
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      await onSave(project.id, { name: name.trim(), description: description.trim() })
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the project")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={!!project} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>Edit project</DialogTitle>
          </DialogHeader>

          <AnimatedField id="project-edit-name" label="Name" value={name} onChange={setName} autoFocus />
          <AnimatedField
            id="project-edit-description"
            label="Description"
            value={description}
            onChange={setDescription}
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
