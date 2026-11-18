import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, within } from "storybook/test"
import { IDS } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { pageOf } from "@/stories/support/interactions"
import { Route as HomeRoute } from "@/src/routes/index"
import { Route as ProjectRoute } from "@/src/routes/project.$projectId"

const meta = {
  title: "Pages/Projects/Overview",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** A project's page: its composer, its one thread, and the Environments plugin. */
export const Default: Story = {
  parameters: at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(
      await canvas.findByRole("heading", { name: "faber" }, { timeout: 5000 }),
    ).toBeVisible()
    await expect(await canvas.findByRole("switch", { name: /Disable Environments/ })).toBeVisible()
    await expect(
      canvas.getAllByText("Why is the probe test flaky?").length,
    ).toBeGreaterThan(0)
  },
}

/** Writing on a project's page starts a thread in that project and opens it. */
export const StartThread: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const input = await canvas.findByRole("textbox", {}, { timeout: 5000 })
    await userEvent.type(input, "hello there{Enter}")
    await expect(
      await canvas.findByText(/comes from the Storybook mock API/, {}, { timeout: 8000 }),
    ).toBeInTheDocument()
  },
}

/** Creating a project goes through the top bar's nav drawer below `md`. */
export const CreateDialog: Story = {
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
