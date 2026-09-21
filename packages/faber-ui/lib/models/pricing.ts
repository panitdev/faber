/**
 * What a model costs, read from its own definition.
 *
 * Stored under `capabilities.pricing` as US dollars per **million** tokens.
 * `capabilities` is a free-form JSON column the API validates one key of at a
 * time, so everything here is defensive: a row that says nothing, or says
 * something this build doesn't recognize, prices nothing rather than failing
 * to render.
 */

import type { ModelConfig } from "@/lib/api"

/** The key under `capabilities` that carries the prices. */
export const PRICING_KEY = "pricing"

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

/** The prices a model's `capabilities` carries, or nothing when it states none. */
export function pricingOf(model: ModelConfig | null | undefined): Pricing {
  if (!model) return EMPTY
  return pricingFrom(model.capabilities)
}

/** The same read, against a raw `capabilities` blob. */
export function pricingFrom(capabilities: unknown): Pricing {
  if (typeof capabilities !== "object" || capabilities === null || Array.isArray(capabilities)) {
    return EMPTY
  }
  const value = (capabilities as Record<string, unknown>)[PRICING_KEY]
  if (typeof value !== "object" || value === null || Array.isArray(value)) return EMPTY

  const v = value as Record<string, unknown>
  return {
    input: price(v.input),
    output: price(v.output),
    cache_read: price(v.cache_read),
    cache_write: price(v.cache_write),
  }
}

/**
 * Writes the prices into a `capabilities` blob without disturbing the rest of
 * it — the model form owns this one key, not the column. An all-unset price
 * drops the key rather than storing four nulls.
 */
export function withPricing(capabilities: unknown, pricing: Pricing): Record<string, unknown> {
  const base =
    typeof capabilities === "object" && capabilities !== null && !Array.isArray(capabilities)
      ? { ...(capabilities as Record<string, unknown>) }
      : {}

  if (!hasPricing(pricing)) {
    delete base[PRICING_KEY]
    return base
  }

  base[PRICING_KEY] = Object.fromEntries(
    KEYS.filter((key) => pricing[key] !== null).map((key) => [key, pricing[key]]),
  )
  return base
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
