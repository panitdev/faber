import type { Meta, StoryObj } from "@storybook/tanstack-react"
import * as React from "react"
import { expect, userEvent, within } from "storybook/test"

import type { Project } from "@/lib/api"
import { IDS } from "@/mocks"
import { Button } from "@charlotte-ui/button"

import {
  CreateProjectDialog,
  DeleteProjectDialog,
  EditProjectDialog,
  RenameProjectDialog,
} from "./project-dialogs"

/**
 * Project dialogs, driven by the state their parent owns: a target means open,
 * `null` means closed. The harness mirrors that, and leaves a Reopen button
 * behind after closing so the exit animation can be watched again without
 * reloading the story. Play stops with the dialog open; close it by hand.
 *
 * These are component stories because rename and delete are reached through
 * the sidebar, which is hidden below `md`, and stories run at a phone width.
 * The page-level wiring lives in `stories/app.stories.tsx`.
 */
const meta = {
  title: "Dialogs/Project",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

const project: Project = {
  id: IDS.projectFaber,
  name: "faber",
  description: "The agent service and its UI.",
  owner_id: IDS.user,
  rev: 3,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
  bindings: [],
}

type Mode = "create" | "rename" | "edit" | "delete"

function Harness({ mode }: { mode: Mode }) {
  const [open, setOpen] = React.useState(true)
  const [target, setTarget] = React.useState<Project | null>(mode === "create" ? null : project)

  const handleOpenChange = (next: boolean) => {
    if (next) return
    setOpen(false)
    setTarget(null)
  }

  const reopen = () => {
    setTarget(mode === "create" ? null : project)
    setOpen(true)
  }

  return (
    <div className="flex min-h-64 items-center justify-center p-6">
      {open ? null : (
        <Button variant="outline" onClick={reopen}>
          Reopen dialog
        </Button>
      )}

      {mode === "create" ? (
        <CreateProjectDialog
          open={open}
          onOpenChange={handleOpenChange}
          onCreate={async () => project}
        />
      ) : mode === "rename" ? (
        <RenameProjectDialog
          project={target}
          onOpenChange={handleOpenChange}
          onRename={async () => project}
        />
      ) : mode === "edit" ? (
        <EditProjectDialog
          project={target}
          onOpenChange={handleOpenChange}
          onSave={async () => project}
        />
      ) : (
        <DeleteProjectDialog
          project={target}
          threadCount={2}
          onOpenChange={handleOpenChange}
          onDelete={async () => {}}
        />
      )}
    </div>
  )
}

/** A new project starts blank; closing it leaves a Reopen affordance behind. */
export const CreateProject: Story = {
  render: () => <Harness mode="create" />,
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body)
    const dialog = within(await page.findByRole("dialog", { name: "New project" }))
    await userEvent.type(dialog.getByLabelText("Name"), "Story project")
  },
}

/** The rename form seeds itself from the project it names. */
export const RenameProject: Story = {
  render: () => <Harness mode="rename" />,
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body)
    const dialog = within(await page.findByRole("dialog", { name: "Rename project" }))
    await expect(dialog.getByLabelText("Name")).toHaveValue("faber")
  },
}

/** Editing carries both the name and the description. */
export const EditProject: Story = {
  render: () => <Harness mode="edit" />,
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body)
    const dialog = within(await page.findByRole("dialog", { name: "Edit project" }))
    await expect(dialog.getByLabelText("Name")).toHaveValue("faber")
    await expect(dialog.getByLabelText("Description")).toHaveValue(
      "The agent service and its UI.",
    )
  },
}

/** A delete confirm names the project and states how many threads go with it. */
export const DeleteProject: Story = {
  render: () => <Harness mode="delete" />,
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body)
    const dialog = within(await page.findByRole("alertdialog", { name: "Delete faber?" }))
    await expect(dialog.getByText(/all 2 of its threads/)).toBeInTheDocument()
  },
}
