/**
 * The thinking knob: what a model offers, and what a session can pick.
 *
 * `params` is a free-form JSON column the API validates one key of at a time,
 * so everything here is defensive: a row that says nothing, or says something
 * this build doesn't recognize, is a model with no thinking knob rather than a
 * page that fails to render.
 */

import type { Effort, ModelConfig, ThinkingCapability, ThinkingSelection } from "@/lib/api"

/** The key under `params` that carries the knob. */
export const THINKING_KEY = "thinking"

export const EFFORTS: Effort[] = ["minimal", "low", "medium", "high", "xhigh", "max"]

const EMPTY: ThinkingCapability = { supported: false, efforts: [], default_effort: null }

function isEffort(value: unknown): value is Effort {
  return typeof value === "string" && (EFFORTS as string[]).includes(value)
}

/** A stored or served knob, read defensively; anything malformed is no knob. */
function parseCapability(value: unknown): ThinkingCapability {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return EMPTY

  const v = value as Record<string, unknown>
  if (v.supported !== true) return EMPTY

  const efforts = Array.isArray(v.efforts) ? v.efforts.filter(isEffort) : []
  return {
    supported: true,
    efforts,
    default_effort: isEffort(v.default_effort) ? v.default_effort : null,
  }
}

/**
 * What the picker offers for a model — `supported: false` means no knob.
 *
 * The API resolves it: the model's own `params.thinking` when it states one,
 * else what its preset says the provider offers. Read from `params` only when
 * the response carries no resolved knob — an API that predates it.
 */
export function thinkingOf(model: ModelConfig | null | undefined): ThinkingCapability {
  if (!model) return EMPTY
  if (model.thinking !== undefined) return parseCapability(model.thinking)
  const params = model.params
  if (typeof params !== "object" || params === null || Array.isArray(params)) {
    return EMPTY
  }
  return parseCapability((params as Record<string, unknown>)[THINKING_KEY])
}

/**
 * Writes the knob into a `params` blob without disturbing the rest of it — the
 * model form owns this one key, not the column.
 */
export function withThinking(
  params: unknown,
  thinking: ThinkingCapability,
): Record<string, unknown> {
  const base =
    typeof params === "object" && params !== null && !Array.isArray(params)
      ? { ...(params as Record<string, unknown>) }
      : {}

  if (!thinking.supported) {
    delete base[THINKING_KEY]
    return base
  }

  base[THINKING_KEY] = {
    supported: true,
    ...(thinking.efforts.length > 0 ? { efforts: thinking.efforts } : {}),
    // Only meaningful as one of the offered levels; the API refuses anything
    // else, so a stale default is dropped here rather than sent to be rejected.
    ...(thinking.default_effort && thinking.efforts.includes(thinking.default_effort)
      ? { default_effort: thinking.default_effort }
      : {}),
  }
  return base
}

/** What a model can be set to, in the order the picker shows it. */
export function selectionsFor(capability: ThinkingCapability): ThinkingSelection[] {
  if (!capability.supported) return []
  // A model with no levels of its own still has an on/off knob; one with
  // levels says "on" by naming a level.
  return capability.efforts.length > 0 ? ["off", ...capability.efforts] : ["off", "on"]
}

const LABELS: Record<ThinkingSelection, string> = {
  off: "Off",
  on: "On",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
}

/**
 * How a selection reads in the picker. `null` is the model's own default.
 *
 * Terse because the label is always shown under a Thinking heading — the
 * trigger beside the model name, or a row in the thinking submenu.
 */
export function selectionLabel(selection: ThinkingSelection | null): string {
  return selection ? LABELS[selection] : "Default"
}
