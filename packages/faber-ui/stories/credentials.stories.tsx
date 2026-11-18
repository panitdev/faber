import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, within } from "storybook/test"
import { at, fast } from "@/stories/support/page"
import { pageOf } from "@/stories/support/interactions"
import { Route as CredentialsRoute } from "@/src/routes/credentials"

const meta = {
  title: "Pages/Credentials",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

export const Default: Story = {
  parameters: at(CredentialsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText("openrouter", {}, { timeout: 5000 })).toBeVisible()
  },
}

/** A credential's delete confirm names the credential it will drop. */
export const DeleteDialog: Story = {
  parameters: { ...at(CredentialsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete openrouter" }, { timeout: 5000 }),
    )
    await pageOf(canvasElement).findByRole("alertdialog", { name: "Delete openrouter?" })
  },
}
