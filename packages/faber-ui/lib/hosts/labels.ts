/**
 * How a host row is worded on screen.
 *
 * One rule drives everything here: **a probe is an observation, never a
 * status.** Everything below phrases the last probe in the past tense with its
 * age attached, so there is no "online" for a caller to accidentally gate on.
 * Per `internal-docs/host.md`, the authoritative answer to "is it reachable"
 * is the next connection attempt.
 */

import type { Host, HostProbe } from "@/lib/api"

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** "3h ago" / "just now". Coarse on purpose — this is an age, not a clock. */
export function age(iso: string, now: number = Date.now()): string {
  const elapsed = now - new Date(iso).getTime()
  if (!Number.isFinite(elapsed)) return "at an unknown time"
  if (elapsed < MINUTE) return "just now"
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)}m ago`
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)}h ago`
  return `${Math.floor(elapsed / DAY)}d ago`
}

/**
 * The last probe as a sentence about the past.
 *
 * Never returns anything a caller could read as current state. A host that has
 * never been probed has no observation at all — `null`, for the caller to
 * render as an empty cell — rather than a default of either "up" or "down".
 */
export function observation(probe: HostProbe | null): string | null {
  if (!probe) return null
  if (probe.ok) return `Last reachable ${age(probe.probed_at)}`
  return `Last attempt ${age(probe.probed_at)}: ${probe.error ?? "failed"}`
}

/** The probe's capability manifest as sorted `name version` pairs. */
export function toolList(probe: HostProbe | null): Array<[string, string]> {
  if (!probe?.tools || typeof probe.tools !== "object" || Array.isArray(probe.tools)) {
    return []
  }
  return Object.entries(probe.tools)
    .map(([name, version]) => [name, String(version)] as [string, string])
    .sort(([a], [b]) => a.localeCompare(b))
}

/**
 * How the host is addressed, for the line under its name. `null` for an agent
 * host: it has no address by construction — faber never dials it — and its
 * transport badge already says so.
 */
export function addressLabel(host: Host): string | null {
  if (host.transport === "ssh") return host.ssh_address ?? "ssh"
  if (host.transport === "agent") return null
  return "This machine"
}
