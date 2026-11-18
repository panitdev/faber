import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, waitFor, within } from "storybook/test"
import { IDS, db } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { Route as ProjectRoute } from "@/src/routes/project.$projectId"

const meta = {
  title: "Pages/Projects/Agents",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/**
 * A project's agents: the built-in one and the owner's own, made, edited and
 * deleted from the page. A name already taken is refused on the name field.
 */
export const ManageAgents: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const page = within(document.body)
    const section = await canvas.findByRole("region", { name: "Agents" }, { timeout: 5000 })
    const agents = within(section)
    await expect(await agents.findByText("general-purpose")).toBeVisible()
    await expect(agents.getByText("reviewer")).toBeVisible()

    // A name the project already has is refused against the name field.
    await userEvent.click(agents.getByRole("button", { name: "New agent" }))
    await userEvent.type(await page.findByLabelText("Name"), "reviewer")
    await userEvent.type(page.getByLabelText("Description"), "Another reviewer.")
    await userEvent.click(await page.findByRole("button", { name: "Create" }))
    await expect(await page.findByText("the project already has an agent with that name")).toHaveAttribute(
      "id",
      "agent-name-error",
    )

    // Renamed, it goes through and is listed.
    await userEvent.clear(page.getByLabelText("Name"))
    await userEvent.type(page.getByLabelText("Name"), "critic")
    await userEvent.click(await page.findByRole("button", { name: "Create" }))
    await waitFor(() => expect(agents.getByText("critic")).toBeVisible())
    // The page is hidden from queries while a dialog is up.
    await waitFor(() => expect(page.queryByRole("dialog")).toBeNull())
    await expect(db().agentDefinitions[IDS.projectFaber].map((row) => row.name)).toContain("critic")

    // Edited, then deleted.
    await userEvent.click(agents.getByRole("button", { name: "More actions for critic" }))
    await userEvent.click(await page.findByRole("menuitem", { name: "Edit" }))
    const description = await page.findByLabelText("Description")
    await userEvent.clear(description)
    await userEvent.type(description, "Finds faults in a plan.")
    await userEvent.click(page.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(agents.getByText(/Finds faults in a plan\./)).toBeVisible())
    await waitFor(() => expect(page.queryByRole("dialog")).toBeNull())

    await userEvent.click(agents.getByRole("button", { name: "More actions for critic" }))
    await userEvent.click(await page.findByRole("menuitem", { name: "Delete" }))
    await userEvent.click(await page.findByRole("button", { name: "Delete" }))
    await waitFor(() => expect(agents.queryByText("critic")).toBeNull())
  },
}
