/**
 * A session's token and cost totals, folded from its turns.
 *
 * Tokens are exact — every persisted assistant message carries what the
 * provider reported. Cost prefers the provider's own figure where it reports
 * one (aggregator endpoints such as OpenRouter do), and falls back to the
 * prices configured on the model otherwise. A total that leans on either a
 * computed price or a missing one is flagged `approximate`.
 */

import type { ModelConfig } from "@/lib/api"
import { costOf, pricingOf } from "@/lib/models/pricing"
import type { Turn } from "@/lib/thread/transcript"

export type UsageSummary = {
  inputTokens: number
  outputTokens: number
  /**
   * Tokens read from the prompt cache. Cache *writes* are tracked too but not
   * shown: they are billed into {@link UsageSummary.cost}, which is where a
   * cache-write price actually lands.
   */
  cacheReadTokens: number
  cacheWriteTokens: number
  /**
   * Reported inside `outputTokens` by every provider that sends it, so it is
   * shown as its own figure but never added to a token sum.
   */
  reasoningTokens: number
  /** Cost in USD, or `null` when nothing in the scope could be priced. */
  cost: number | null
  /**
   * True when the figure is not fully authoritative: some of it was computed
   * from configured prices, or some reported usage had no cost at all — so
   * {@link UsageSummary.cost} is a best effort, not a bill.
   */
  approximate: boolean
  /** Whether any usage was reported at all — the card's visibility gate. */
  reported: boolean
}

const EMPTY: UsageSummary = {
  inputTokens: 0,
  outputTokens: 0,
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
  reasoningTokens: 0,
  cost: null,
  approximate: false,
  reported: false,
}

export function summarizeUsage(turns: Turn[], models: ModelConfig[]): UsageSummary {
  const byAlias = new Map(models.map((model) => [model.alias, model]))

  let inputTokens = 0
  let outputTokens = 0
  let cacheReadTokens = 0
  let cacheWriteTokens = 0
  let reasoningTokens = 0
  let cost = 0
  let anyCost = false
  let approximate = false
  let anyUsage = false

  for (const turn of turns) {
    const usage = turn.usage
    if (!usage) continue
    anyUsage = true

    inputTokens += usage.inputTokens
    outputTokens += usage.outputTokens
    cacheReadTokens += usage.cacheReadTokens
    cacheWriteTokens += usage.cacheWriteTokens
    reasoningTokens += usage.reasoningTokens

    // The provider's own figure wins where the whole turn reported one.
    if (usage.costed && usage.cost !== null) {
      cost += usage.cost
      anyCost = true
      continue
    }

    // Otherwise the turn carries no authoritative cost: fall back to the
    // model's configured prices. Either way the total is no longer exact.
    approximate = true
    const model = usage.model ? byAlias.get(usage.model) : undefined
    const computed = model ? costOf(usage, pricingOf(model)) : null
    if (computed !== null) {
      cost += computed
      anyCost = true
    }
  }

  if (!anyUsage) return EMPTY

  return {
    inputTokens,
    outputTokens,
    cacheReadTokens,
    cacheWriteTokens,
    reasoningTokens,
    cost: anyCost ? cost : null,
    approximate,
    reported: true,
  }
}
