import type { Meta, StoryObj, TanStackParameters } from "@storybook/tanstack-react"
import type { AnyRoute } from "@tanstack/react-router"
import { expect, userEvent, within } from "storybook/test"

import { IDS, type MockApiParameters } from "@/mocks"
import "@/src/routeTree.gen"
import { Route as CredentialsRoute } from "@/src/routes/credentials"
import { Route as EnvironmentsRoute } from "@/src/routes/environments"
import { Route as HomeRoute } from "@/src/routes/index"
import { Route as ModelsRoute } from "@/src/routes/models"
import { Route as ProjectRoute } from "@/src/routes/project.$projectId"
import { Route as SessionRoute } from "@/src/routes/session.$sessionId"

/**
 * The dialogs a page opens, driven from the real app frame against the mock
 * API: rename and delete a thread, create a project, edit a plugin's config,
 * and the delete confirms behind hosts, models, credentials, and containers.
 *
 * Each play function stops with the dialog open, so it stays on screen to
 * inspect. Close it by hand to watch the exit animation.
 *
 * Project rename and delete are sidebar-only and live next to their components
 * in `components/shell/project-dialogs.stories.tsx`.
 */
const meta = {
  title: "Dialogs",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** Renders the story at `route`, inside the app's real route tree. */
const at = (route: AnyRoute, extra: { params?: Record<string, string> } = {}) =>
  ({ tanstack: { router: { route, ...extra } } }) as TanStackParameters

const fast: MockApiParameters = { latency: 0, replySpeed: 0 }

/** Portalled menus and dialogs land on `document.body`, outside the story canvas. */
function pageOf(canvasElement: HTMLElement) {
  return within(canvasElement.ownerDocument.body)
}

/** Opens the top bar's actions drawer for the thread on screen. */
async function openSessionActions(canvasElement: HTMLElement, label: string) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole("button", { name: `Actions for ${label}` }, { timeout: 5000 }),
  )
  return within(
    await pageOf(canvasElement).findByRole("dialog", { name: label }),
  )
}

/** Opens a nav row's own-actions menu. The trigger is hover-only in CSS. */
async function openRowMenu(canvasElement: HTMLElement, label: string) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole(
      "button",
      { name: `More actions for ${label}` },
      { timeout: 5000 },
    ),
  )
  return pageOf(canvasElement)
}

/** Opens the built-in Environments plugin settings from its row menu. */
async function openEnvironmentsSettings(canvasElement: HTMLElement) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole("button", { name: /More actions for Environments/ }, { timeout: 5000 }),
  )
  const page = pageOf(canvasElement)
  await userEvent.click(await page.findByRole("menuitem", { name: /Settings/ }))
  return within(await page.findByRole("dialog"))
}

/** Renaming a thread goes through the top bar's actions drawer below `md`. */
export const RenameSessionDialog: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionRefactor } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const actions = await openSessionActions(
      canvasElement,
      "Split host dialogs into their own module",
    )
    await userEvent.click(await actions.findByRole("button", { name: "Rename" }))
    const page = pageOf(canvasElement)
    const dialog = within(await page.findByRole("dialog", { name: "Rename thread" }))
    await expect(dialog.getByLabelText("Title")).toHaveValue(
      "Split host dialogs into their own module",
    )
  },
}

/** Deleting a thread names the thread the confirm will take with it. */
export const DeleteSessionDialog: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionRefactor } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const actions = await openSessionActions(
      canvasElement,
      "Split host dialogs into their own module",
    )
    await userEvent.click(await actions.findByRole("button", { name: "Delete" }))
    const page = pageOf(canvasElement)
    await page.findByRole("alertdialog", { name: /Delete Split host dialogs/ })
  },
}

/** Creating a project goes through the top bar's nav drawer below `md`. */
export const CreateProjectDialog: Story = {
  parameters: { ...at(HomeRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Open menu" }, { timeout: 5000 }),
    )
    const page = pageOf(canvasElement)
    const nav = within(await page.findByRole("dialog", { name: "Panit" }))
    await userEvent.click(await nav.findByRole("button", { name: "New project" }))
    const dialog = within(await page.findByRole("dialog", { name: "New project" }))
    await userEvent.type(dialog.getByLabelText("Name"), "Story project")
  },
}

/** The Environments plugin's built-in settings form. */
export const PluginSettingsDialog: Story = {
  parameters: { ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }), mockApi: fast },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    await expect(dialog.getByRole("button", { name: "Save" })).toBeVisible()
  },
}

/** Unbinding a plugin confirms against the plugin's own name. */
export const ProjectUnbindPluginDialog: Story = {
  parameters: { ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }), mockApi: fast },
  play: async ({ canvasElement }) => {
    const page = await openRowMenu(canvasElement, "Environments")
    await userEvent.click(await page.findByRole("menuitem", { name: "Remove" }))
    await page.findByRole("alertdialog", { name: "Remove Environments?" })
  },
}

/** A model's delete confirm names the alias it will drop. */
export const DeleteModelDialog: Story = {
  parameters: { ...at(ModelsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete opus" }, { timeout: 5000 }),
    )
    await pageOf(canvasElement).findByRole("alertdialog", { name: "Delete opus?" })
  },
}

/** A credential's delete confirm names the credential it will drop. */
export const DeleteCredentialDialog: Story = {
  parameters: { ...at(CredentialsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete openrouter" }, { timeout: 5000 }),
    )
    await pageOf(canvasElement).findByRole("alertdialog", { name: "Delete openrouter?" })
  },
}

/** A host's delete confirm names the host it will drop. */
export const DeleteHostDialog: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const page = await openRowMenu(canvasElement, "buildbox")
    await userEvent.click(await page.findByRole("menuitem", { name: "Delete host" }))
    await page.findByRole("alertdialog", { name: "Delete buildbox?" })
  },
}

/** Destroying a container names the container label the server will remove. */
export const ContainerDeleteDialog: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete faber-dev" }, { timeout: 5000 }),
    )
    await pageOf(canvasElement).findByRole("alertdialog", { name: "Delete dev?" })
  },
}

/** Unregistering falls back to the container ref when it has no label. */
export const ContainerUnregisterDialog: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole(
        "button",
        { name: "Unregister 3f9c2a1b7d04" },
        { timeout: 5000 },
      ),
    )
    await pageOf(canvasElement).findByRole(
      "alertdialog",
      { name: "Unregister 3f9c2a1b7d04?" },
    )
  },
}

/** The add-host flow opens on its transport chooser. */
export const AddHostDialog: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Add host" }, { timeout: 5000 }),
    )
    const dialog = within(
      await pageOf(canvasElement).findByRole("dialog", { name: "Add host" }),
    )
    await expect(await dialog.findByText("How should Panit reach this host?")).toBeInTheDocument()
  },
}

/** Adopting an existing container opens the registration form. */
export const AddContainerDialog: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole(
        "button",
        { name: "Add a container to buildbox" },
        { timeout: 5000 },
      ),
    )
    const page = pageOf(canvasElement)
    await userEvent.click(await page.findByRole("menuitem", { name: /^Add/ }))
    await page.findByRole("dialog", { name: "Add a container on buildbox" })
  },
}

/** Creating a container starts it from an image instead of adopting one. */
export const CreateContainerDialog: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole(
        "button",
        { name: "Add a container to buildbox" },
        { timeout: 5000 },
      ),
    )
    const page = pageOf(canvasElement)
    await userEvent.click(await page.findByRole("menuitem", { name: /^Create/ }))
    await page.findByRole("dialog", { name: "Create a container on buildbox" })
  },
}
