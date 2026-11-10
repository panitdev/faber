import type { Meta, StoryObj } from "@storybook/tanstack-react"
import * as React from "react"
import { expect, userEvent, within } from "storybook/test"

import {
  ThinkingEffort,
  type ThinkingEffortLevel,
  type ThinkingEffortProps,
} from "./thinking-effort"

/** The caller owns both the stops and their names — nothing is inferred. */
const LEVELS: ThinkingEffortLevel[] = [
  { value: "off", label: "Off" },
  { value: "low", label: "Light" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra high" },
  { value: "max", label: "Max" },
]

/** Portalled popovers land on `document.body`, outside the story canvas. */
const pageOf = (canvasElement: HTMLElement) =>
  within(canvasElement.ownerDocument.body)

/** Keeps the pick in local state so the slider is live in Storybook. */
function Harness({ value: initial, ...props }: ThinkingEffortProps) {
  const [value, setValue] = React.useState(initial)
  return (
    <div className="flex min-h-64 w-full items-center justify-center">
      <ThinkingEffort {...props} value={value} onChange={setValue} side="bottom" />
    </div>
  )
}

const meta = {
  component: ThinkingEffort,
  tags: ["ai-generated"],
  args: {
    levels: LEVELS,
    value: null,
    defaultValue: "medium",
    defaultLabel: "Default",
    title: "claude-opus-5",
    onChange: () => {},
  },
  render: (args) => <Harness {...args} />,
} satisfies Meta<typeof ThinkingEffort>

export default meta
type Story = StoryObj<typeof meta>

/** Unset: parked on the caller's default level, with reset disabled. */
export const Default: Story = {}

/** An explicit pick, so the reset control has something to clear. */
export const PickedLevel: Story = {
  args: { value: "xhigh" },
}

/** Labels and header rendering are the caller's, not a built-in ladder. */
export const CustomDisplay: Story = {
  args: {
    levels: [
      { value: "minimal", label: "Minimal" },
      { value: "medium", label: "Medium" },
      {
        value: "xhigh",
        label: "Extra high",
        display: (
          <span>
            Extra high{" "}
            <span className="text-xs font-medium text-muted-foreground">
              slower
            </span>
          </span>
        ),
      },
    ],
    value: "xhigh",
  },
}

/** Two stops are a two-stop slider, not a special case. */
export const OnOff: Story = {
  args: {
    levels: [
      { value: "off", label: "Off" },
      { value: "on", label: "On" },
    ],
    value: null,
    defaultValue: null,
    title: "gpt-4.1",
  },
}

/** Nothing is rendered for a caller that supplies no levels. */
export const NoLevels: Story = {
  args: { levels: [] },
}

export const SnapToMax: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await userEvent.click(canvas.getByRole("button", { name: /Thinking effort/ }))

    const page = pageOf(canvasElement)
    const slider = await page.findByRole("slider", { name: "Thinking effort" })
    slider.focus()
    await userEvent.keyboard("{End}")

    // The header follows the thumb, and the level is now a real pick.
    await expect(page.getByText("Max")).toBeVisible()
    await expect(
      page.getByRole("button", { name: "Use the model's default effort" }),
    ).toBeEnabled()
  },
}
