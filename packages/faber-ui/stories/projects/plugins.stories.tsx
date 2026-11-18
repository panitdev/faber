import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, fireEvent, userEvent, waitFor, within } from "storybook/test"
import { http, HttpResponse } from "msw"
import { IDS } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { openRowMenu, openEnvironmentsSettings } from "@/stories/support/interactions"
import { Route as ProjectRoute } from "@/src/routes/project.$projectId"

const meta = {
  title: "Pages/Projects/Plugins",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** Turning a plugin off flips the switch at once and is saved without a `rev`. */
export const Disable: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const toggle = await canvas.findByRole("switch", { name: /Disable Environments/ }, { timeout: 5000 })
    await userEvent.click(toggle)
    await waitFor(() =>
      expect(canvas.getByRole("switch", { name: /Enable Environments/ })).not.toBeChecked(),
    )
  },
}

/** Adding an environment picks a registered one and saves the whole list against `rev`. */
export const Settings: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    const page = within(canvasElement.ownerDocument.body)
    // Stories run at a phone width: the row's name opens its editor and the
    // rest of its actions sit in its menu.
    await expect(await dialog.findByRole("button", { name: "scratch" })).toBeInTheDocument()
    await userEvent.click(dialog.getByRole("button", { name: /Add environment/ }))
    const editor = within(await page.findByRole("dialog", { name: "Add environment" }))
    await expect(editor.getByLabelText("Name")).toHaveValue("laptop")
    await userEvent.click(editor.getByRole("button", { name: "Add" }))
    await waitFor(() => expect(page.queryByRole("dialog", { name: "Add environment" })).toBeNull())
    await userEvent.click(await dialog.findByRole("button", { name: "More actions for laptop" }))
    await userEvent.click(await page.findByRole("menuitem", { name: "Move up" }))
    // The open menu hides the rest of the page from the accessibility tree.
    await waitFor(() => expect(page.queryByRole("menu")).toBeNull())
    const names = dialog
      .getAllByRole("row")
      .slice(1)
      .map((row) => row.querySelector("td:nth-child(3) button")?.textContent)
    await expect(names).toEqual(["laptop", "scratch"])
    await userEvent.click(dialog.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(page.queryByRole("dialog")).toBeNull())
  },
}

/** Dragging a row's handle reorders the list, just as the move buttons do. */
export const SettingsDragToOrder: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    const page = within(canvasElement.ownerDocument.body)
    await userEvent.click(dialog.getByRole("button", { name: /Add environment/ }))
    const editor = within(await page.findByRole("dialog", { name: "Add environment" }))
    await userEvent.click(editor.getByRole("button", { name: "Add" }))
    await waitFor(() => expect(page.queryByRole("dialog", { name: "Add environment" })).toBeNull())

    const names = () =>
      dialog
        .getAllByRole("row")
        .slice(1)
        .map((row) => row.querySelector("td:nth-child(3) button")?.textContent)
    // Retried: the list stays out of the accessibility tree until the add
    // dialog's close animation finishes.
    await waitFor(() => expect(names()).toEqual(["scratch", "laptop"]))

    // Dropping on the top half of the first row puts the dragged row above it.
    const rows = dialog.getAllByRole("row").slice(1)
    const dataTransfer = new DataTransfer()
    const handle = rows[1].querySelector('[draggable="true"]')!
    fireEvent.dragStart(handle, { dataTransfer })
    fireEvent.dragOver(rows[0], { dataTransfer, clientY: rows[0].getBoundingClientRect().top + 1 })
    fireEvent.drop(rows[0], { dataTransfer })
    fireEvent.dragEnd(handle, { dataTransfer })

    await waitFor(() => expect(names()).toEqual(["laptop", "scratch"]))
    await userEvent.click(dialog.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(page.queryByRole("dialog")).toBeNull())
  },
}

/** A config the plugin refuses comes back onto the field it points at. */
export const SettingsRejected: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    const page = within(canvasElement.ownerDocument.body)
    await userEvent.click(await dialog.findByRole("button", { name: "scratch" }))
    let editor = within(await page.findByRole("dialog", { name: "Configure scratch" }))
    const name = editor.getByLabelText("Name")
    await userEvent.clear(name)
    await userEvent.type(name, "a b")
    await userEvent.click(editor.getByRole("button", { name: "Apply" }))
    await userEvent.click(await dialog.findByRole("button", { name: "Save" }))
    // The refusal marks the row, and shows on the field once it is opened.
    await userEvent.click(await dialog.findByRole("button", { name: "Needs fixing a b" }))
    editor = within(await page.findByRole("dialog", { name: "Configure a b" }))
    await expect(await editor.findByText(/starting with a letter or digit/)).toBeVisible()
  },
}

/**
 * The server refuses the config with a fixed 422, as the owner check does for
 * an environment that is not the user's: the refusal shows on its field.
 * Fixed rather than computed by the mock, so the UI is checked against the
 * API's wire shape and not against the mock's copy of the rules.
 */
export const SettingsRejectedByServer: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  beforeEach: ({ msw }) => {
    msw.use(
      http.put("/api/projects/:id/plugins/environments", () =>
        HttpResponse.json(
          {
            error: "the plugin refused this config",
            code: "config-invalid",
            errors: [
              {
                path: "/environments/0/environmentId",
                message: "not one of your registered environments",
              },
            ],
          },
          { status: 422 },
        ),
      ),
    )
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    await userEvent.click(dialog.getByRole("button", { name: "Save" }))
    await expect(
      await dialog.findByText(/not one of your registered environments/),
    ).toBeVisible()
  },
}

/** The Environments plugin's built-in settings form. */
export const SettingsDialog: Story = {
  parameters: { ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }), mockApi: fast },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    await expect(dialog.getByRole("button", { name: "Save" })).toBeVisible()
  },
}

/** Unbinding a plugin confirms against the plugin's own name. */
export const RemoveDialog: Story = {
  parameters: { ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }), mockApi: fast },
  play: async ({ canvasElement }) => {
    const page = await openRowMenu(canvasElement, "Environments")
    await userEvent.click(await page.findByRole("menuitem", { name: "Remove" }))
    await page.findByRole("alertdialog", { name: "Remove Environments?" })
  },
}
