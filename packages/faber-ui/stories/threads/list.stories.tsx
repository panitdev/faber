import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, waitFor, within } from "storybook/test"
import { at, fast } from "@/stories/support/page"
import { Route as ThreadsRoute } from "@/src/routes/threads"

const meta = {
  title: "Pages/Threads/List",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** Every thread in one list, across projects and the workspace. */
export const Default: Story = {
  parameters: at(ThreadsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(
      await canvas.findByRole("heading", { name: "Threads" }, { timeout: 5000 }),
    ).toBeVisible()
    await expect(
      (await canvas.findAllByText("Split host dialogs into their own module")).length,
    ).toBeGreaterThan(0)
  },
}

/** Selecting threads raises the action bar; deleting removes just those rows. */
export const Selection: Story = {
  parameters: { ...at(ThreadsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByRole("heading", { name: "Threads" }, { timeout: 5000 })
    const boxes = await canvas.findAllByRole("checkbox", { name: /^Select / }, { timeout: 5000 })
    const before = boxes.length
    await expect(before).toBeGreaterThan(1)
    await expect(canvas.queryByRole("toolbar", { name: "Selected threads" })).toBeNull()

    await userEvent.click(boxes[0])
    await userEvent.click(boxes[1])
    const toolbar = await canvas.findByRole("toolbar", { name: "Selected threads" })
    await expect(within(toolbar).getByText("2 selected")).toBeVisible()

    await userEvent.click(within(toolbar).getByRole("button", { name: "Delete" }))
    const dialog = within(document.body)
    await expect(await dialog.findByText("Delete 2 threads?")).toBeVisible()
    await userEvent.click(await dialog.findByRole("button", { name: "Delete" }))

    await waitFor(
      () =>
        expect(canvas.getAllByRole("checkbox", { name: /^Select / })).toHaveLength(before - 2),
      { timeout: 5000 },
    )
    await expect(canvas.queryByRole("toolbar", { name: "Selected threads" })).toBeNull()
  },
}
