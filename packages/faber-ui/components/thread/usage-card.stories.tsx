import type { Meta, StoryObj } from "@storybook/tanstack-react"
import * as React from "react"
import { expect } from "storybook/test"

import { UsageCard } from "./usage-card"
import type { UsageSummary } from "@/lib/thread/usage"

const meta = {
  component: UsageCard,
  tags: ["ai-generated"],
} satisfies Meta<typeof UsageCard>

export default meta
type Story = StoryObj<typeof meta>

const all: UsageSummary = {
  inputTokens: 1_204_000,
  outputTokens: 96_000,
  cacheReadTokens: 1_180_000,
  cacheWriteTokens: 12_000,
  reasoningTokens: 40_000,
  cost: 4.21,
  approximate: false,
  reported: true,
}

const last: UsageSummary = {
  inputTokens: 128_400,
  outputTokens: 21_300,
  cacheReadTokens: 96_000,
  cacheWriteTokens: 4_200,
  reasoningTokens: 8_100,
  cost: 0.87,
  approximate: false,
  reported: true,
}

const empty: UsageSummary = {
  inputTokens: 0,
  outputTokens: 0,
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
  reasoningTokens: 0,
  cost: null,
  approximate: false,
  reported: false,
}

/** Cost reported upstream — exact, so no `≈`. */
export const UpstreamCost: Story = {
  args: { all, last },
}

/** Priced from the model's configured rates — a best effort. */
export const EstimatedCost: Story = {
  args: {
    all: { ...all, cost: 3.14, approximate: true },
    last: { ...last, cost: 0.42, approximate: true },
  },
}

/** Tokens reported, no price to apply. */
export const Unpriced: Story = {
  args: {
    all: { ...all, cost: null, approximate: true },
    last: { ...last, cost: null, approximate: true },
  },
}

export const SwitchesScope: Story = {
  args: { all, last },
  play: async ({ canvas, userEvent }) => {
    const lastTab = canvas.getByRole("tab", { name: "Last turn" })
    await userEvent.click(lastTab)
    await expect(lastTab).toHaveAttribute("aria-selected", "true")
  },
}

/** Figures ticking up, so the per-field slide is visible. */
export const LiveUpdates: Story = {
  args: { all, last },
  render: (args) => {
    const [tick, setTick] = React.useState(0)
    React.useEffect(() => {
      const id = setInterval(() => setTick((value) => value + 1), 1200)
      return () => clearInterval(id)
    }, [])
    const bump = (summary: UsageSummary): UsageSummary => ({
      ...summary,
      inputTokens: summary.inputTokens + tick * 137,
      outputTokens: summary.outputTokens + tick * 41,
      cacheReadTokens: summary.cacheReadTokens + tick * 311,
      reasoningTokens: summary.reasoningTokens + tick * 7,
      cost: summary.cost === null ? null : summary.cost + tick * 0.004,
    })
    return <UsageCard all={bump(args.all)} last={bump(args.last)} className={args.className} />
  },
}

export const Empty: Story = {
  args: { all: empty, last: empty },
}
