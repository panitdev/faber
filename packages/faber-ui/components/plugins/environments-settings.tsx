import * as React from "react"
import { Laptop, Plus, Trash2 } from "lucide-react"

import type { Host, JsonValue } from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
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
 * The Environments plugin's settings: scratch and the repos cloned into it,
 * and the user's own machines. The config's shape and every rule on it are
 * the plugin's (`crates/api/src/plugins/environments/config.rs`); this form
 * only lays it out, and the plugin's refusals come back onto its fields.
 */

interface Repo {
  url: string
  /** Empty means the remote's default branch. */
  ref: string
  dir: string
}

interface Machine {
  /** Stable across renames: the plugin reports a rename by it. */
  id: string
  name: string
  /** Empty until a computer is picked. */
  agentId: string
  /** Empty means unset. */
  workdir: string
}

interface Draft {
  scratch: { enabled: boolean; repos: Repo[] }
  machines: Machine[]
}

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
  const scratch = (typeof root.scratch === "object" && root.scratch !== null ? root.scratch : {}) as Record<
    string,
    unknown
  >
  return {
    scratch: {
      enabled: scratch.enabled !== false,
      repos: records(scratch.repos).map((repo) => ({
        url: text(repo.url),
        ref: text(repo.ref),
        dir: text(repo.dir),
      })),
    },
    machines: records(root.machines).map((machine) => ({
      id: text(machine.id),
      name: text(machine.name),
      agentId: text(machine.agentId),
      workdir: text(machine.workdir),
    })),
  }
}

function write(draft: Draft): JsonValue {
  return {
    scratch: {
      enabled: draft.scratch.enabled,
      repos: draft.scratch.repos.map((repo) => ({
        url: repo.url.trim(),
        ...(repo.ref.trim() ? { ref: repo.ref.trim() } : {}),
        dir: repo.dir.trim(),
      })),
    },
    machines: draft.machines.map((machine) => ({
      id: machine.id,
      name: machine.name.trim(),
      agentId: machine.agentId,
      ...(machine.workdir.trim() ? { workdir: machine.workdir.trim() } : {}),
    })),
  }
}

/** The directory a clone of `url` would get: its last path segment, less `.git`. */
function dirFor(url: string): string {
  const last = url.trim().replace(/\/+$/, "").split(/[/:]/).pop() ?? ""
  return last.replace(/\.git$/, "")
}

/** A machine name that is not taken yet, from the computer's own name. */
function nameFor(host: Host | undefined, taken: string[]): string {
  const base =
    (host?.name ?? "machine")
      .toLowerCase()
      .replace(/[^a-z0-9._-]+/g, "-")
      .replace(/^[^a-z0-9]+/, "")
      .slice(0, 60) || "machine"
  if (base !== "scratch" && !taken.includes(base)) return base
  for (let n = 2; ; n++) {
    if (!taken.includes(`${base}-${n}`)) return `${base}-${n}`
  }
}

function fieldPaths(draft: Draft): Set<string> {
  const paths = new Set<string>()
  draft.scratch.repos.forEach((_, i) => {
    for (const field of ["url", "ref", "dir"]) paths.add(`/scratch/repos/${i}/${field}`)
  })
  draft.machines.forEach((_, i) => {
    for (const field of ["name", "agentId", "workdir"]) paths.add(`/machines/${i}/${field}`)
  })
  return paths
}

function EnvironmentsForm({ value, onChange, errors, disabled }: SettingsFormProps<Draft>) {
  const { hosts, loaded } = useHosts()
  // Environments reach a machine only through a connected faber-agent.
  const computers = hosts.filter((host) => host.transport === "agent")

  const setRepo = (index: number, next: Repo) =>
    onChange({
      ...value,
      scratch: {
        ...value.scratch,
        repos: value.scratch.repos.map((repo, i) => (i === index ? next : repo)),
      },
    })
  const setMachine = (index: number, next: Machine) =>
    onChange({
      ...value,
      machines: value.machines.map((machine, i) => (i === index ? next : machine)),
    })

  const known = fieldPaths(value)
  const unplaced = [...errors].filter(([path]) => !known.has(path))

  return (
    <div className="flex flex-col gap-6">
      <section className="flex flex-col gap-3">
        <div className="flex items-start gap-3">
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <label htmlFor="env-scratch" className="text-sm font-medium">
              Scratch
            </label>
            <p className="text-xs text-muted-foreground">
              A sandbox Faber provides. It's a cache, not storage: idle scratch is cleared, and
              repos are cloned again on next use.
            </p>
          </div>
          <Switch
            id="env-scratch"
            size="sm"
            checked={value.scratch.enabled}
            disabled={disabled}
            onCheckedChange={(enabled) =>
              onChange({ ...value, scratch: { ...value.scratch, enabled } })
            }
          />
        </div>

        {value.scratch.enabled ? (
          <div className="flex flex-col gap-3">
            {value.scratch.repos.map((repo, index) => (
              <Row
                key={index}
                label={`Repository ${index + 1}`}
                disabled={disabled}
                onRemove={() =>
                  onChange({
                    ...value,
                    scratch: {
                      ...value.scratch,
                      repos: value.scratch.repos.filter((_, i) => i !== index),
                    },
                  })
                }
              >
                <Input
                  id={`env-repo-${index}-url`}
                  label="URL"
                  value={repo.url}
                  placeholder="https://github.com/owner/repo.git"
                  error={errors.get(`/scratch/repos/${index}/url`)}
                  disabled={disabled}
                  onChange={(url) =>
                    // The directory follows the URL until the user names it.
                    setRepo(index, {
                      ...repo,
                      url,
                      dir: repo.dir === dirFor(repo.url) ? dirFor(url) : repo.dir,
                    })
                  }
                />
                <div className="grid gap-3 sm:grid-cols-2">
                  <Input
                    id={`env-repo-${index}-dir`}
                    label="Directory"
                    value={repo.dir}
                    hint={repo.dir ? `Cloned to /repos/${repo.dir}` : undefined}
                    error={errors.get(`/scratch/repos/${index}/dir`)}
                    disabled={disabled}
                    onChange={(dir) => setRepo(index, { ...repo, dir })}
                  />
                  <Input
                    id={`env-repo-${index}-ref`}
                    label="Ref (optional)"
                    value={repo.ref}
                    placeholder="default branch"
                    error={errors.get(`/scratch/repos/${index}/ref`)}
                    disabled={disabled}
                    onChange={(ref) => setRepo(index, { ...repo, ref })}
                  />
                </div>
              </Row>
            ))}
            <Button
              type="button"
              variant="outline"
              size="xs"
              className="self-start"
              disabled={disabled}
              onClick={() =>
                onChange({
                  ...value,
                  scratch: {
                    ...value.scratch,
                    repos: [...value.scratch.repos, { url: "", ref: "", dir: "" }],
                  },
                })
              }
            >
              <Plus />
              Add repository
            </Button>
          </div>
        ) : null}
      </section>

      <section className="flex flex-col gap-3">
        <div className="flex flex-col gap-0.5">
          <h3 className="text-sm font-medium">Machines</h3>
          <p className="text-xs text-muted-foreground">
            Your own computers running faber-agent. The agent addresses each by its name here.
          </p>
        </div>

        {value.machines.map((machine, index) => (
          <Row
            key={machine.id}
            label={machine.name || `Machine ${index + 1}`}
            disabled={disabled}
            onRemove={() =>
              onChange({ ...value, machines: value.machines.filter((_, i) => i !== index) })
            }
          >
            <div className="grid gap-3 sm:grid-cols-2">
              <Input
                id={`env-machine-${index}-name`}
                label="Name"
                value={machine.name}
                error={errors.get(`/machines/${index}/name`)}
                disabled={disabled}
                onChange={(name) => setMachine(index, { ...machine, name })}
              />
              <div className="w-full">
                <label
                  htmlFor={`env-machine-${index}-agent`}
                  className="mb-1.5 block text-sm font-medium text-foreground/80"
                >
                  Computer
                </label>
                <Select
                  value={machine.agentId}
                  disabled={disabled}
                  onValueChange={(agentId) => setMachine(index, { ...machine, agentId })}
                >
                  <SelectTrigger
                    id={`env-machine-${index}-agent`}
                    aria-invalid={errors.has(`/machines/${index}/agentId`) || undefined}
                  >
                    <SelectValue placeholder={loaded ? "Pick a computer" : "Loading…"} />
                  </SelectTrigger>
                  <SelectContent>
                    {computers.map((host) => (
                      <SelectItem key={host.id} value={host.id}>
                        {host.name}
                        {host.disabled_at ? " (disabled)" : ""}
                      </SelectItem>
                    ))}
                    {/* Still selectable as it is, so an unknown id is not
                        silently swapped for another computer. */}
                    {loaded && machine.agentId && !computers.some((host) => host.id === machine.agentId) ? (
                      <SelectItem value={machine.agentId}>Unknown computer</SelectItem>
                    ) : null}
                  </SelectContent>
                </Select>
                <FieldError message={errors.get(`/machines/${index}/agentId`)} />
              </div>
            </div>
            <Input
              id={`env-machine-${index}-workdir`}
              label="Working directory (optional)"
              value={machine.workdir}
              placeholder="/home/me/project"
              hint="This project's directory there; commands start in it."
              error={errors.get(`/machines/${index}/workdir`)}
              disabled={disabled}
              onChange={(workdir) => setMachine(index, { ...machine, workdir })}
            />
          </Row>
        ))}

        {loaded && computers.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            No computers yet. Install faber-agent on one from Environments, then add it here.
          </p>
        ) : null}

        <Button
          type="button"
          variant="outline"
          size="xs"
          className="self-start"
          disabled={disabled || computers.length === 0}
          onClick={() => {
            const taken = value.machines.map((machine) => machine.name)
            // Prefer a computer this project doesn't use yet.
            const host =
              computers.find((c) => !value.machines.some((m) => m.agentId === c.id)) ?? computers[0]
            onChange({
              ...value,
              machines: [
                ...value.machines,
                { id: crypto.randomUUID(), name: nameFor(host, taken), agentId: host?.id ?? "", workdir: "" },
              ],
            })
          }}
        >
          <Laptop />
          Add machine
        </Button>
      </section>

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

function Row({
  label,
  disabled,
  onRemove,
  children,
}: {
  label: string
  disabled: boolean
  onRemove: () => void
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-3 rounded-2xl border border-border/60 p-3">
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-sm font-medium">{label}</span>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={`Remove ${label}`}
          disabled={disabled}
          onClick={onRemove}
        >
          <Trash2 className="size-4" />
        </Button>
      </div>
      {children}
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
  description: "Where threads in this project can read files and run commands.",
}

