import * as React from "react"
import { ArrowDown, ArrowUp, Box, Plus, Trash2 } from "lucide-react"

import { faber, type EnvironmentCandidate, type JsonValue } from "@/lib/api"
import { AnimatedField } from "@/components/ui/animated-field"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import type { BuiltInSettings, SettingsFormProps } from "./plugin-settings"

/**
 * The Environments plugin's settings: one ordered list, at most one scratch
 * and any number of the user's registered environments. The order is the
 * order the agent sees them in. The config's shape and every rule on it are
 * the plugin's (`crates/api/src/plugins/environments/config.rs`); this form
 * only lays it out, and the plugin's refusals come back onto its fields.
 */

interface Entry {
  /** Stable across renames: the plugin reports a rename by it. */
  id: string
  /** What the agent passes as `execute_in`. */
  name: string
  enabled: boolean
  type: "scratch" | "user"
  /** For a `user` entry: a candidate's id. Empty until one is picked. */
  environmentId: string
}

interface Draft {
  environments: Entry[]
}

const SCRATCH: Entry = { id: "scratch", name: "scratch", enabled: true, type: "scratch", environmentId: "" }

function text(value: unknown): string {
  return typeof value === "string" ? value : ""
}

function records(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((item): item is Record<string, unknown> => typeof item === "object" && item !== null)
    : []
}

function read(config: JsonValue): Draft {
  const root = (typeof config === "object" && config !== null && !Array.isArray(config) ? config : {}) as Record<
    string,
    unknown
  >
  // A config from before the list (or none at all) reads as the default.
  if (!Array.isArray(root.environments)) return { environments: [{ ...SCRATCH }] }
  return {
    environments: records(root.environments).map((entry) => ({
      id: text(entry.id),
      name: text(entry.name),
      enabled: entry.enabled !== false,
      type: entry.type === "scratch" ? "scratch" : "user",
      environmentId: text(entry.environmentId),
    })),
  }
}

function write(draft: Draft): JsonValue {
  return {
    environments: draft.environments.map((entry) => ({
      id: entry.id,
      name: entry.name.trim(),
      enabled: entry.enabled,
      type: entry.type,
      ...(entry.type === "user" ? { environmentId: entry.environmentId } : {}),
    })),
  }
}

/** A name that is not taken yet, from the environment's own label. */
function nameFor(label: string, taken: string[]): string {
  const base =
    label
      .toLowerCase()
      .replace(/[^a-z0-9._-]+/g, "-")
      .replace(/^[^a-z0-9]+/, "")
      .slice(0, 60) || "environment"
  if (!taken.includes(base)) return base
  for (let n = 2; ; n++) {
    if (!taken.includes(`${base}-${n}`)) return `${base}-${n}`
  }
}

const FIELDS = ["id", "name", "enabled", "type"]

/** The pointers an entry's fields show errors at. An error anywhere else —
 * an `environmentId` on a scratch entry, which has no picker — is listed
 * below the entries instead of being dropped. */
function fieldPaths(draft: Draft): Set<string> {
  const paths = new Set<string>()
  draft.environments.forEach((entry, i) => {
    for (const field of FIELDS) paths.add(`/environments/${i}/${field}`)
    if (entry.type === "user") paths.add(`/environments/${i}/environmentId`)
  })
  return paths
}

/** Every environment the caller registered, as `GET /api/environments` lists them. */
function useCandidates(): { candidates: EnvironmentCandidate[]; loaded: boolean } {
  const [candidates, setCandidates] = React.useState<EnvironmentCandidate[]>([])
  const [loaded, setLoaded] = React.useState(false)
  React.useEffect(() => {
    let cancelled = false
    void faber
      .listEnvironments()
      .then((found) => {
        if (!cancelled) setCandidates(found)
      })
      // The picker shows the ids it has; a failed load leaves them as they
      // are rather than blocking the rest of the form.
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setLoaded(true)
      })
    return () => {
      cancelled = true
    }
  }, [])
  return { candidates, loaded }
}

function describe(candidate: EnvironmentCandidate): string {
  const where = candidate.kind === "container" ? `container on ${candidate.host_name}` : "host"
  return `${candidate.label} (${where})${candidate.disabled ? ", disabled" : ""}`
}

function EnvironmentsForm({ value, onChange, errors, disabled }: SettingsFormProps<Draft>) {
  const { candidates, loaded } = useCandidates()
  const list = value.environments
  const hasScratch = list.some((entry) => entry.type === "scratch")

  const set = (index: number, next: Entry) =>
    onChange({ environments: list.map((entry, i) => (i === index ? next : entry)) })
  const move = (index: number, by: -1 | 1) => {
    const to = index + by
    if (to < 0 || to >= list.length) return
    const next = [...list]
    ;[next[index], next[to]] = [next[to], next[index]]
    onChange({ environments: next })
  }
  const taken = list.map((entry) => entry.name)

  const known = fieldPaths(value)
  const unplaced = [...errors].filter(([path]) => !known.has(path))

  return (
    <div className="flex flex-col gap-3">
      {list.map((entry, index) => {
        const at = (field: string) => errors.get(`/environments/${index}/${field}`)
        const label = entry.name || (entry.type === "scratch" ? "Scratch" : `Environment ${index + 1}`)
        return (
          <div key={entry.id || index} className="flex flex-col gap-3 rounded-2xl border border-border/60 p-3">
            <div className="flex items-center gap-2">
              <Switch
                id={`env-${index}-enabled`}
                size="sm"
                aria-label={`Turn ${label} ${entry.enabled ? "off" : "on"}`}
                checked={entry.enabled}
                disabled={disabled}
                onCheckedChange={(enabled) => set(index, { ...entry, enabled })}
              />
              <span className="min-w-0 flex-1 truncate text-sm font-medium">
                {label}
                <span className="ml-1.5 text-xs font-normal text-muted-foreground">
                  {entry.type === "scratch" ? "scratch" : "your environment"}
                </span>
              </span>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Move ${label} up`}
                disabled={disabled || index === 0}
                onClick={() => move(index, -1)}
              >
                <ArrowUp className="size-4" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Move ${label} down`}
                disabled={disabled || index === list.length - 1}
                onClick={() => move(index, 1)}
              >
                <ArrowDown className="size-4" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Remove ${label}`}
                disabled={disabled}
                onClick={() => onChange({ environments: list.filter((_, i) => i !== index) })}
              >
                <Trash2 className="size-4" />
              </Button>
            </div>

            {entry.type === "scratch" ? (
              <p className="text-xs text-muted-foreground">
                A sandbox Faber provides to each session. Its files last as long as the session.
              </p>
            ) : null}

            <div className={entry.type === "user" ? "grid gap-3 sm:grid-cols-2" : "grid gap-3"}>
              <Input
                id={`env-${index}-name`}
                label="Name"
                value={entry.name}
                hint="What the agent calls it."
                error={at("name")}
                disabled={disabled}
                onChange={(name) => set(index, { ...entry, name })}
              />
              {entry.type === "user" ? (
                <div className="w-full">
                  <label
                    htmlFor={`env-${index}-environment`}
                    className="mb-1.5 block text-sm font-medium text-foreground/80"
                  >
                    Environment
                  </label>
                  <Select
                    value={entry.environmentId}
                    disabled={disabled}
                    onValueChange={(environmentId) => set(index, { ...entry, environmentId })}
                  >
                    <SelectTrigger
                      id={`env-${index}-environment`}
                      aria-invalid={Boolean(at("environmentId")) || undefined}
                    >
                      <SelectValue placeholder={loaded ? "Pick an environment" : "Loading…"} />
                    </SelectTrigger>
                    <SelectContent>
                      {candidates.map((candidate) => (
                        <SelectItem key={candidate.id} value={candidate.id}>
                          {describe(candidate)}
                        </SelectItem>
                      ))}
                      {/* Still selectable as it is, so an unknown id is not
                          silently swapped for another environment. */}
                      {loaded && entry.environmentId && !candidates.some((c) => c.id === entry.environmentId) ? (
                        <SelectItem value={entry.environmentId}>Unknown environment</SelectItem>
                      ) : null}
                    </SelectContent>
                  </Select>
                  <FieldError message={at("environmentId")} />
                </div>
              ) : null}
            </div>
            <FieldError message={at("type") ?? at("id") ?? at("enabled")} />
          </div>
        )
      })}

      {loaded && candidates.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          No environments of your own yet. Register a host or a container from Environments, then add it
          here.
        </p>
      ) : null}

      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          size="xs"
          disabled={disabled || candidates.length === 0}
          onClick={() => {
            // Prefer one this project doesn't use yet.
            const candidate =
              candidates.find((c) => !list.some((entry) => entry.environmentId === c.id)) ?? candidates[0]
            onChange({
              environments: [
                ...list,
                {
                  id: crypto.randomUUID(),
                  name: nameFor(candidate?.label ?? "environment", taken),
                  enabled: true,
                  type: "user",
                  environmentId: candidate?.id ?? "",
                },
              ],
            })
          }}
        >
          <Plus />
          Add environment
        </Button>
        {hasScratch ? null : (
          <Button
            type="button"
            variant="outline"
            size="xs"
            disabled={disabled}
            onClick={() =>
              onChange({
                environments: [
                  ...list,
                  { ...SCRATCH, name: taken.includes("scratch") ? nameFor("scratch", taken) : "scratch" },
                ],
              })
            }
          >
            <Box />
            Add scratch
          </Button>
        )}
      </div>

      {unplaced.length > 0 ? (
        <ul className="flex flex-col gap-1 text-sm text-destructive">
          {unplaced.map(([path, message]) => (
            <li key={path}>
              {path ? <code className="mr-1 text-xs">{path}</code> : null}
              {message}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}

function Input({
  error,
  ...props
}: React.ComponentProps<typeof AnimatedField> & { error?: string }) {
  return (
    <div className="w-full">
      <AnimatedField {...props} hint={error ? undefined : props.hint} />
      <FieldError message={error} />
    </div>
  )
}

function FieldError({ message }: { message?: string }) {
  return message ? <p className="mt-1.5 text-xs text-destructive">{message}</p> : null
}

export const environmentsSettings: BuiltInSettings<Draft> = {
  read,
  write,
  Form: EnvironmentsForm,
  description: "Where threads in this project can read files and run commands, in the order the agent sees them.",
}
