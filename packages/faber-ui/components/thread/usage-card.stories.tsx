import type { Meta, StoryObj } from "@storybook/tanstack-react"

import { UsageCard } from "./usage-card"
import type { UsageSummary } from "@/lib/thread/usage"

const meta = {
  component: UsageCard,
  tags: ["ai-generated"],
} satisfies Meta<typeof UsageCard>

export default meta
type Story = StoryObj<typeof meta>

const base: UsageSummary = {
  inputTokens: 128_400,
  outputTokens: 21_300,
  cacheReadTokens: 96_000,
  cacheWriteTokens: 4_200,
  reasoningTokens: 8_100,
  totalTokens: 249_900,
  cost: 0.87,
  approximate: false,
}

/** The provider reported the cost — exact, so no `≈`. */
export const UpstreamCost: Story = {
  args: { summary: base },
}

/** Priced from the model's configured rates — a best effort. */
export const EstimatedCost: Story = {
  args: { summary: { ...base, cost: 0.42, approximate: true } },
}

/** Tokens reported, no price to apply. */
export const Unpriced: Story = {
  args: { summary: { ...base, cost: null, approximate: true } },
}

export const Empty: Story = {
  args: {
    summary: {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      reasoningTokens: 0,
      totalTokens: 0,
      cost: null,
      approximate: false,
    },
  },
}
