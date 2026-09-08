import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect } from "storybook/test"

import { FaberMark } from "./faber-mark"

const meta = {
  component: FaberMark,
  tags: ["ai-generated"],
} satisfies Meta<typeof FaberMark>

export default meta
type Story = StoryObj<typeof meta>

export const Idle: Story = {
  play: async ({ canvasElement }) => {
    const mark = canvasElement.querySelector('[aria-hidden="true"]')
    await expect(mark).toBeInTheDocument()
  },
}

export const Working: Story = { args: { working: true } }

export const Large: Story = { args: { size: 48 } }
