/**
 * Reading a model's catalog description.
 *
 * The catalog states image input as a modality rather than a capability flag,
 * so "vision" is derived here, once, rather than at every call site.
 */

import type { ModelSpec } from "@/lib/api"

/** Whether the model takes images as input. */
export function hasVision(spec: Pick<ModelSpec, "modalities">): boolean {
  return spec.modalities.input.some((modality) => modality.toLowerCase() === "image")
}
