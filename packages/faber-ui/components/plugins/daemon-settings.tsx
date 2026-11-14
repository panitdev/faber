import * as React from "react"

import {
  faber,
  FaberError,
  type BindPluginRequest,
  type DaemonInfo,
  type ModelConfig,
  type PluginType,
  type Project,
  type ProjectBinding,
} from "@/lib/api"
import { Button } from "@charlotte-ui/button"
import { Input } from "@charlotte-ui/input"
import { Switch } from "@charlotte-ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@charlotte-ui/select"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@charlotte-ui/dialog"
import { cn } from "@charlotte-ui/utils"

/** Whether a plugin type runs a daemon that opens sessions of its own. */
export function hasDaemon(type: PluginType | undefined): boolean {
  return !!type?.capabilities.daemon
}

/** Bytes as a person reads them. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`
}

/** One line for a daemon's state, for the binding row. */
export function DaemonBadge({ info }: { info: DaemonInfo | undefined }) {
  const daemon = info?.daemon
  if (!daemon || daemon.state === "running" || daemon.state === "stopped") return null
  return (
    <span
      title={daemon.error}
      className={cn(
        "shrink-0 text-xs",
        daemon.state === "degraded" ? "text-destructive" : "text-muted-foreground",
      )}
    >
      {daemon.state === "degraded" ? "Stopped after errors" : "Restarting"}
    </span>
  )
}

const NO_MODEL = "__none__"

interface Draft {
  model: string
  sessions: string
  runs: string
  perMinute: string
  /** `null`: every plugin. */
  plugins: string[] | null
}

function draftOf(binding: ProjectBinding): Draft {
  const text = (value: number | null) => (value === null ? "" : String(value))
  return {
    model: binding.model ?? NO_MODEL,
    sessions: text(binding.limits.sessions),
    runs: text(binding.limits.runs),
    perMinute: text(binding.limits.messages_per_minute),
    plugins: binding.session_plugins,
  }
}

/** `null` for blank (no limit), a number for a whole positive one, `undefined` for neither. */
function limit(text: string): number | null | undefined {
  const trimmed = text.trim()
  if (!trimmed) return null
  const value = Number(trimmed)
  return Number.isInteger(value) && value > 0 ? value : undefined
}

/**
 * What the sessions a binding's daemon opens may do, and how the daemon is
 * doing. Those sessions are driven by people in the plugin's channel, not by
 * the owner, so the owner picks their model, caps what they may start, and
 * may keep some of the project's plugins out of their reach.
 */
export function DaemonSettingsDialog({
  project,
  binding,
  name,
  types,
  onOpenChange,
  onSave,
}: {
  project: Project
  binding: ProjectBinding | null
  name: string
  types: PluginType[]
  onOpenChange: (open: boolean) => void
  /** Stores the change; throws the API's refusal. */
  onSave: (type: string, body: BindPluginRequest) => Promise<void>
}) {
  return (
    <Dialog open={!!binding} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-xl">
        {binding ? (
          <DaemonForm
            key={binding.type}
            project={project}
            binding={binding}
            name={name}
            types={types}
            onSave={onSave}
            onDone={() => onOpenChange(false)}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  )
}

function DaemonForm({
  project,
  binding,
  name,
  types,
  onSave,
  onDone,
}: {
  project: Project
  binding: ProjectBinding
  name: string
  types: PluginType[]
  onSave: (type: string, body: BindPluginRequest) => Promise<void>
  onDone: () => void
}) {
  const [draft, setDraft] = React.useState(() => draftOf(binding))
  const [models, setModels] = React.useState<ModelConfig[] | null>(null)
  const [info, setInfo] = React.useState<DaemonInfo | null>(null)
  const [error, setError] = React.useState<string | null>(null)
  const [saving, setSaving] = React.useState(false)
  const [retrying, setRetrying] = React.useState(false)

  const load = React.useCallback(
    () =>
      faber.getDaemon(project.id, binding.type).then(setInfo, () => setInfo(null)),
    [project.id, binding.type],
  )

  React.useEffect(() => {
    void faber.listModels().then(setModels, () => setModels([]))
    void load()
  }, [load])

  const others = project.bindings.filter((other) => other.type !== binding.type)
  const nameOf = (type: string) => types.find((found) => found.id === type)?.name ?? type
  const limited = draft.plugins !== null

  const retry = async () => {
    setRetrying(true)
    try {
      await faber.retryDaemon(project.id, binding.type)
      await load()
    } finally {
      setRetrying(false)
    }
  }

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    const limits = {
      sessions: limit(draft.sessions),
      runs: limit(draft.runs),
      messages_per_minute: limit(draft.perMinute),
    }
    if (Object.values(limits).some((value) => value === undefined)) {
      setError("Limits are whole numbers of 1 or more, or blank for no limit.")
      return
    }
    setSaving(true)
    setError(null)
    try {
      await onSave(binding.type, {
        model: draft.model === NO_MODEL ? null : draft.model,
        limits: limits as BindPluginRequest["limits"],
        session_plugins: draft.plugins,
      })
      onDone()
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "the change didn't go through")
    } finally {
      setSaving(false)
    }
  }

  const daemon = info?.daemon
  return (
    <form onSubmit={handleSubmit} className="flex min-h-0 flex-col gap-5">
      <DialogHeader>
        <DialogTitle>{name}: threads it starts</DialogTitle>
        <DialogDescription>
          People in {name} start these threads, and they run on your models. Choose what they run
          on and what they can reach.
        </DialogDescription>
      </DialogHeader>

      <section className="flex flex-col gap-1.5 text-sm">
        <div className="flex items-center gap-2">
          <span className="text-muted-foreground">Status</span>
          <span className={cn(daemon?.state === "degraded" && "text-destructive")}>
            {!info
              ? "Loading…"
              : daemon?.state === "running"
                ? "Running"
                : daemon?.state === "restarting"
                  ? `Restarting (${daemon.failures} failures)`
                  : daemon?.state === "degraded"
                    ? "Stopped after repeated errors"
                    : "Not running"}
          </span>
          {daemon?.state === "degraded" ? (
            <Button
              type="button"
              size="sm"
              variant="outline"
              loading={retrying}
              onClick={() => void retry()}
              className="ml-auto"
            >
              Retry
            </Button>
          ) : null}
        </div>
        {daemon && "error" in daemon ? (
          <p className="break-words text-xs text-muted-foreground">{daemon.error}</p>
        ) : null}
        {info ? (
          <p className="text-xs text-muted-foreground">
            Stored data: {formatBytes(info.storage.used)} of {formatBytes(info.storage.allowed)}
          </p>
        ) : null}
      </section>

      <div className="flex flex-col gap-1.5">
        <label htmlFor="daemon-model" className="text-sm">
          Model
        </label>
        <Select value={draft.model} onValueChange={(model) => setDraft({ ...draft, model })}>
          <SelectTrigger id="daemon-model">
            <SelectValue placeholder={models ? "Pick a model" : "Loading…"} />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={NO_MODEL}>None: messages wait</SelectItem>
            {(models ?? []).map((model) => (
              <SelectItem key={model.id} value={model.alias}>
                {model.alias}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {draft.model === NO_MODEL ? (
          <p className="text-xs text-muted-foreground">
            Until you pick one, messages from {name} are kept but nothing answers them.
          </p>
        ) : null}
      </div>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Input
          id="daemon-sessions"
          label="Open threads"
          placeholder="No limit"
          value={draft.sessions}
          disabled={saving}
          onChange={(text) => setDraft({ ...draft, sessions: text })}
        />
        <Input
          id="daemon-runs"
          label="Runs at once"
          placeholder="No limit"
          value={draft.runs}
          disabled={saving}
          onChange={(text) => setDraft({ ...draft, runs: text })}
        />
        <Input
          id="daemon-rate"
          label="Messages a minute"
          placeholder="No limit"
          value={draft.perMinute}
          disabled={saving}
          onChange={(text) => setDraft({ ...draft, perMinute: text })}
        />
      </div>

      <section className="flex flex-col gap-2">
        <div className="flex items-center gap-3">
          <span className="flex-1 text-sm">Limit which plugins these threads can use</span>
          <Switch
            size="sm"
            checked={limited}
            onCheckedChange={(on) => setDraft({ ...draft, plugins: on ? [] : null })}
            aria-label="Limit which plugins these threads can use"
          />
        </div>
        {limited ? (
          others.length === 0 ? (
            <p className="text-xs text-muted-foreground">This project has no other plugins.</p>
          ) : (
            <ul>
              {others.map((other) => {
                const allowed = draft.plugins?.includes(other.type) ?? false
                return (
                  <li
                    key={other.type}
                    className="flex h-9 items-center gap-3 border-t border-border/60 text-sm"
                  >
                    <span className="flex-1">{nameOf(other.type)}</span>
                    <Switch
                      size="sm"
                      checked={allowed}
                      onCheckedChange={(on) =>
                        setDraft({
                          ...draft,
                          plugins: on
                            ? [...(draft.plugins ?? []), other.type]
                            : (draft.plugins ?? []).filter((type) => type !== other.type),
                        })
                      }
                      aria-label={`Allow ${nameOf(other.type)}`}
                    />
                  </li>
                )
              })}
            </ul>
          )
        ) : (
          <p className="text-xs text-muted-foreground">
            They can use every plugin in this project, including its environments.
          </p>
        )}
      </section>

      {error ? <p className="text-sm text-destructive">{error}</p> : null}

      <DialogFooter>
        <Button type="submit" loading={saving} loadingText="Saving">
          Save
        </Button>
      </DialogFooter>
    </form>
  )
}
