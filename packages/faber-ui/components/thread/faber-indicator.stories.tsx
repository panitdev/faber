import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect } from "storybook/test"

import { FaberIndicator } from "./faber-indicator"

const meta = {
  component: FaberIndicator,
  tags: ["ai-generated"],
} satisfies Meta<typeof FaberIndicator>

export default meta
type Story = StoryObj<typeof meta>

export const Idle: Story = {}

export const Working: Story = {
  args: { working: true },
  play: async ({ canvas }) => {
    // Only rendered while `working` — proves the verb line actually mounted.
    const verbs = ["Working", "Figuring", "Shaping", "Refining", "Thinking", "Considering", "Forming", "Weaving"]
    const text = await canvas.findByText(new RegExp(verbs.join("|")))
    await expect(text).toBeVisible()
  },
}
