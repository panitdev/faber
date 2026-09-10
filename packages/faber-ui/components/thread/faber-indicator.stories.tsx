import type { Meta, StoryObj } from "@storybook/tanstack-react"
import * as React from "react"
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
    // The hidden width-measurer duplicates the word (aria-hidden), so scope
    // to the visible tray text.
    const text = await canvas.findByText(new RegExp(verbs.join("|")), {
      selector: "span:not([aria-hidden])",
    })
    await expect(text).toBeVisible()
  },
}

export const CustomText: Story = {
  args: { working: true, text: "Searching" },
  play: async ({ canvas }) => {
    const text = await canvas.findByText("Searching", { selector: "span:not([aria-hidden])" })
    await expect(text).toBeVisible()
  },
}

const TOGGLE_VERBS = ["Working", "Figuring", "Shaping", "Refining", "Thinking", "Considering", "Forming", "Weaving"]

export const Toggle: Story = {
  render: (args) => {
    const [working, setWorking] = React.useState(false)
    return (
      <div className="flex flex-col gap-4">
        <button type="button" onClick={() => setWorking((w) => !w)}>
          {working ? "Set idle" : "Set working"}
        </button>
        <FaberIndicator {...args} working={working} />
      </div>
    )
  },
  play: async ({ canvas, userEvent }) => {
    await userEvent.click(canvas.getByRole("button", { name: "Set working" }))
    const text = await canvas.findByText(new RegExp(TOGGLE_VERBS.join("|")), {
      selector: "span:not([aria-hidden])",
    })
    await expect(text).toBeVisible()
    // Let the enter animation settle, then the tray must fully contain the
    // word — guards the stale-width clip ("Working" stuck as "Work").
    await new Promise((resolve) => setTimeout(resolve, 900))
    const tray = text.closest(".overflow-hidden")
    await expect(tray?.scrollWidth ?? 0).toBeLessThanOrEqual((tray?.clientWidth ?? 0) + 1)
  },
}
