import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, within } from "storybook/test"
import { type MockApiParameters } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { pageOf, openRowMenu } from "@/stories/support/interactions"
import { Route as EnvironmentsRoute } from "@/src/routes/environments"

const meta = {
  title: "Pages/Environments",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** Local, SSH + docker, and agent hosts; one with a failed last probe. */
export const Default: Story = {
  parameters: at(EnvironmentsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText("buildbox", {}, { timeout: 5000 })).toBeVisible()
  },
}

export const Empty: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
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
