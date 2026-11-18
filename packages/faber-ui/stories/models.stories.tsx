import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, within } from "storybook/test"
import { type MockApiParameters } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { pageOf } from "@/stories/support/interactions"
import { Route as ModelsRoute } from "@/src/routes/models"

const meta = {
  title: "Pages/Models",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

export const Default: Story = {
  parameters: at(ModelsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findAllByText("opus", {}, { timeout: 5000 })).not.toHaveLength(0)
  },
}

export const Empty: Story = {
  parameters: { ...at(ModelsRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
}

/** A model's delete confirm names the alias it will drop. */
export const DeleteDialog: Story = {
  parameters: { ...at(ModelsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete opus" }, { timeout: 5000 }),
    )
    await pageOf(canvasElement).findByRole("alertdialog", { name: "Delete opus?" })
  },
}
