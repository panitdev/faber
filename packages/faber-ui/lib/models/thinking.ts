/**
 * The thinking knob, read from a model's own definition.
 *
 * `capabilities` is a free-form JSON column the API validates one key of at a
 * time, so everything here is defensive: a row that says nothing, or says
 * something this build doesn't recognize, is a model with no thinking knob
 * rather than a page that fails to render.
 */

import type { Effort, ModelConfig, ThinkingCapability, ThinkingSelection } from "@/lib/api"

/** The key under `capabilities` that carries the knob. */
export const THINKING_KEY = "thinking"

export const EFFORTS: Effort[] = ["low", "medium", "high", "xhigh", "max"]

const EMPTY: ThinkingCapability = { supported: false, efforts: [], default_effort: null }

function isEffort(value: unknown): value is Effort {
  return typeof value === "string" && (EFFORTS as string[]).includes(value)
}

/** What the picker offers for a model — `supported: false` means no knob. */
export function thinkingOf(model: ModelConfig | null | undefined): ThinkingCapability {
  if (!model) return EMPTY
  const capabilities = model.capabilities
  if (typeof capabilities !== "object" || capabilities === null || Array.isArray(capabilities)) {
    return EMPTY
  }
  const value = (capabilities as Record<string, unknown>)[THINKING_KEY]
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
 * Writes the knob into a `capabilities` blob without disturbing the rest of
 * it — the model form owns this one key, not the column.
 */
export function withThinking(
  capabilities: unknown,
  thinking: ThinkingCapability,
): Record<string, unknown> {
  const base =
    typeof capabilities === "object" && capabilities !== null && !Array.isArray(capabilities)
      ? { ...(capabilities as Record<string, unknown>) }
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
  off: "Thinking off",
  on: "Thinking on",
  low: "Think: low",
  medium: "Think: medium",
  high: "Think: high",
  xhigh: "Think: xhigh",
  max: "Think: max",
}

/** How a selection reads in the picker. `null` is the model's own default. */
export function selectionLabel(selection: ThinkingSelection | null): string {
  return selection ? LABELS[selection] : "Default thinking"
}
