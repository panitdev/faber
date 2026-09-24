/**
 * What a model costs, read from the preset that describes it.
 *
 * Prices are US dollars per **million** tokens and live on the model's
 * resolved `preset` (the built-in empty preset when none is linked), so
 * everything here is defensive: a model with no price stated prices nothing
 * rather than failing to render.
 */

import type { ModelConfig } from "@/lib/api"

/** USD per million tokens. `null` means the price is not stated. */
export type Pricing = {
  input: number | null
  output: number | null
  cache_read: number | null
  cache_write: number | null
}

/**
 * The token counts a price is applied to.
 *
 * Structural, so the transcript's own usage shape can be passed without this
 * module depending on it. Reasoning tokens are deliberately absent: providers
 * bill them as output and report them inside it, so pricing them here would
 * count them twice.
 */
export type TokenCounts = {
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
}

const EMPTY: Pricing = { input: null, output: null, cache_read: null, cache_write: null }

const KEYS: (keyof Pricing)[] = ["input", "output", "cache_read", "cache_write"]

function price(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null
}

/** The prices a model's resolved preset carries, or nothing when it states none. */
export function pricingOf(model: ModelConfig | null | undefined): Pricing {
  if (!model) return EMPTY
  const pricing = model.preset?.pricing
  if (typeof pricing !== "object" || pricing === null) return EMPTY
  return {
    input: price(pricing.input),
    output: price(pricing.output),
    cache_read: price(pricing.cache_read),
    cache_write: price(pricing.cache_write),
  }
}

/** Whether any price is stated at all — distinct from a price of zero. */
export function hasPricing(pricing: Pricing): boolean {
  return KEYS.some((key) => pricing[key] !== null)
}

/**
 * The cost of a message's tokens in USD, or `null` when none of its token
 * kinds is priced.
 *
 * Best effort by construction: only the kinds with both a count and a price
 * are billed, so a model that prices input but not cache reads still yields a
 * usable estimate rather than refusing to answer. `null` means there is
 * nothing to divide by, not that the message was free.
 */
export function costOf(tokens: TokenCounts, pricing: Pricing): number | null {
  const perMillion: [number, number | null][] = [
    [tokens.inputTokens, pricing.input],
    [tokens.outputTokens, pricing.output],
    [tokens.cacheReadTokens, pricing.cache_read],
    [tokens.cacheWriteTokens, pricing.cache_write],
  ]

  let total = 0
  let priced = false
  for (const [count, price] of perMillion) {
    if (price === null) continue
    priced = true
    total += (count / 1_000_000) * price
  }
  return priced ? total : null
}
