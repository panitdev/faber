/**
 * Rendering byte and core counts the same way on every page.
 *
 * One rule runs through all of it: **`null` is unlimited, never "unset"**.
 * Unlimited is a word, not a blank.
 */

const GIB = 1024 * 1024 * 1024

/** Bytes as a short size. Unlimited is a word, not a blank. */
export function bytes(value: number | null | undefined): string {
  if (value === null || value === undefined) return "unlimited"
  if (value >= GIB) return `${round(value / GIB)} GiB`
  if (value >= 1024 * 1024) return `${round(value / (1024 * 1024))} MiB`
  return `${value} B`
}

function round(value: number): string {
  return value >= 10 ? String(Math.round(value)) : value.toFixed(1).replace(/\.0$/, "")
}

/** CPU as cores, since a thousandth of a core is not a unit anyone thinks in. */
export function cores(millis: number | null): string {
  if (millis === null) return "unlimited"
  return `${round(millis / 1000)} ${millis === 1000 ? "core" : "cores"}`
}

export function count(value: number | null): string {
  return value === null ? "unlimited" : String(value)
}

