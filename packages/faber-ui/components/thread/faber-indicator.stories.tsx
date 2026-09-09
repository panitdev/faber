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
    const verbs = ["Working", "Figuring", "Shaping", "Refining", "Thinking", "Considering", "Forming", "Weaving"]
    const text = await canvas.findByText(new RegExp(verbs.join("|")))
    await expect(text).toBeVisible()
  },
}

export const CustomText: Story = {
  args: { working: true, text: "Searching" },
  play: async ({ canvas }) => {
    const text = await canvas.findByText("Searching")
    await expect(text).toBeVisible()
  },
}
