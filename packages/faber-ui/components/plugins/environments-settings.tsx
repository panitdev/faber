import * as React from "react"
import { AnimatePresence, motion, useReducedMotion } from "framer-motion"
import {
  AlertCircle,
  ArrowDown,
  ArrowUp,
  Box,
  GripVertical,
  MoreHorizontal,
  Pencil,
  Plus,
  Server,
  Trash2,
} from "lucide-react"

import { cn, DEFAULT_EASE } from "@charlotte-ui/utils"

import { faber, type EnvironmentCandidate, type JsonValue } from "@/lib/api"
import { useLastNonNull } from "@/lib/use-last-non-null"
import { Input } from "@charlotte-ui/input"
import { Button } from "@charlotte-ui/button"
import { Switch } from "@charlotte-ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@charlotte-ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableEmpty,
  TableHead,
  TableHeader,
  TableRow,
  TableRowActions,
} from "@charlotte-ui/table"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@charlotte-ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@charlotte-ui/dropdown-menu"
import type { BuiltInSettings, ConfigErrors, SettingsFormProps } from "./plugin-settings"

/**
 * The Environments plugin's settings: one ordered list, at most one scratch
 * and any number of the user's registered environments. The order is the
 * order the agent sees them in. The config's shape and every rule on it are
 * the plugin's (`crates/api/src/plugins/environments/config.rs`); this form
 * only lays it out, and the plugin's refusals come back onto its fields.
 *
 * The list is a table; each row's fields are edited in a dialog over the
 * settings dialog. That dialog edits a copy, so cancelling it leaves the row
 * as it was, and applying it only changes the draft — the settings dialog's
 * Save is still what stores anything. Rows reorder by dragging their handle;
 * the up/down buttons stay for keyboards, which dragging never serves. A drop
 * (or a button) animates the rows into their new order rather than snapping.
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

/** Rows glide to their new place; short enough not to lag behind a drop. */
const ROW_TRANSITION = { duration: 0.28, ease: DEFAULT_EASE } as const
const DRAG_TRANSITION = { duration: 0.15, ease: DEFAULT_EASE } as const

/** Charlotte's row, still a `<tr>`, with room for layout animation. */
const MotionRow = motion.create(TableRow)

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
 * below the table instead of being dropped. */
function fieldPaths(draft: Draft): Set<string> {
  const paths = new Set<string>()
  draft.environments.forEach((entry, i) => {
    for (const field of FIELDS) paths.add(`/environments/${i}/${field}`)
    if (entry.type === "user") paths.add(`/environments/${i}/environmentId`)
  })
  return paths
}

/** One row's refusals, by field. */
type EntryErrors = Partial<Record<"id" | "name" | "enabled" | "type" | "environmentId", string>>

function entryErrors(errors: ConfigErrors, index: number): EntryErrors {
  const at = (field: string) => errors.get(`/environments/${index}/${field}`)
  return {
    id: at("id"),
    name: at("name"),
    enabled: at("enabled"),
    type: at("type"),
    environmentId: at("environmentId"),
  }
}

function hasErrors(found: EntryErrors): boolean {
  return Object.values(found).some(Boolean)
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

function labelOf(entry: Entry, index: number): string {
  return entry.name || (entry.type === "scratch" ? "Scratch" : `Environment ${index + 1}`)
}

/** What the editor dialog is open on: a row in the list, or one not added yet. */
type Editing = { index: number; entry: Entry } | { index: null; entry: Entry }

function EnvironmentsForm({ value, onChange, errors, disabled }: SettingsFormProps<Draft>) {
  const { candidates, loaded } = useCandidates()
  const [editing, setEditing] = React.useState<Editing | null>(null)
  // Closed clears `editing`; the dialog body stays mounted for the exit
  // animation, so it keeps rendering the entry it was opened on.
  const shownEditing = useLastNonNull(editing)
  const currentEditing = editing ?? shownEditing
  // Bumped on each open, so a row reopened after a cancel starts from the
  // stored copy again instead of the draft the editor was left on.
  const [editingFormKey, setEditingFormKey] = React.useState(0)
  const openEditor = (next: Editing) => {
    setEditingFormKey((key) => key + 1)
    setEditing(next)
  }
  // The row a drag started on, and where it would land. Kept as indices because
  // nothing reorders until the drop, so they stay valid for the whole drag.
  const [drag, setDrag] = React.useState<number | null>(null)
  const [over, setOver] = React.useState<{ index: number; after: boolean } | null>(null)
  const reduce = useReducedMotion()
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
  /** Drop the row at `from` immediately before or after the row at `to`. */
  const reorder = (from: number, to: number, after: boolean) => {
    if (from === to) return
    let at = to + (after ? 1 : 0)
    // Pulling the row out first shifts everything behind it left by one.
    if (from < at) at -= 1
    const next = [...list]
    const [entry] = next.splice(from, 1)
    next.splice(at, 0, entry)
    onChange({ environments: next })
  }
  const endDrag = () => {
    setDrag(null)
    setOver(null)
  }
  const taken = list.map((entry) => entry.name)

  const known = fieldPaths(value)
  const unplaced = [...errors].filter(([path]) => !known.has(path))

  const apply = (entry: Entry) => {
    if (!editing) return
    if (editing.index === null) onChange({ environments: [...list, entry] })
    else set(editing.index, entry)
    setEditing(null)
  }

  const addEnvironment = () => {
    // Prefer one this project doesn't use yet.
    const candidate = candidates.find((c) => !list.some((entry) => entry.environmentId === c.id)) ?? candidates[0]
    openEditor({
      index: null,
      entry: {
        id: crypto.randomUUID(),
        name: nameFor(candidate?.label ?? "environment", taken),
        enabled: true,
        type: "user",
        environmentId: candidate?.id ?? "",
      },
    })
  }

  return (
    <div className="flex flex-col gap-3">
      <Table variant="framed" density="compact">
        <TableHeader>
          <TableRow>
            <TableHead className="w-px">
              <span className="sr-only">Reorder</span>
            </TableHead>
            <TableHead className="w-px">
              <span className="sr-only">On</span>
            </TableHead>
            <TableHead>Name</TableHead>
            <TableHead className="hidden sm:table-cell">Runs in</TableHead>
            <TableHead className="w-px">
              <span className="sr-only">Actions</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 ? (
            <TableEmpty
              colSpan={5}
              icon={<Server className="size-5" />}
              title="No environments"
              description="Threads in this project can't read files or run commands until you add one."
            />
          ) : (
            <AnimatePresence initial={false}>
              {list.map((entry, index) => {
                const label = labelOf(entry, index)
                const rowErrors = entryErrors(errors, index)
                const invalid = hasErrors(rowErrors)
                const droppingHere = over?.index === index && drag !== null && drag !== index
                return (
                  <MotionRow
                    key={entry.id || index}
                    // FLIP the rows into their new order on a drop or a move
                    // button. Skipped when the OS asks for less motion: the list
                    // still reorders, it just doesn't glide.
                    layout={reduce ? false : "position"}
                    initial={false}
                    animate={{ opacity: drag === index ? 0.55 : 1 }}
                    exit={{ opacity: 0 }}
                    transition={{
                      layout: ROW_TRANSITION,
                      opacity: drag === index ? DRAG_TRANSITION : ROW_TRANSITION,
                    }}
                    data-state={invalid ? "selected" : undefined}
                    onDragOver={(event) => {
                      if (drag === null) return
                      event.preventDefault()
                      event.dataTransfer.dropEffect = "move"
                      const rect = event.currentTarget.getBoundingClientRect()
                      // The row's midpoint decides whether the drop goes above
                      // or below it, so a drag can land anywhere in the list.
                      setOver({ index, after: event.clientY > rect.top + rect.height / 2 })
                    }}
                    onDrop={(event) => {
                      if (drag === null) return
                      event.preventDefault()
                      const target = over?.index === index ? over : { index, after: false }
                      reorder(drag, target.index, target.after)
                      endDrag()
                    }}
                    className={cn(
                      "[&>td]:transition-shadow",
                      droppingHere &&
                        (over.after
                          ? "[&>td]:shadow-[inset_0_-2px_0_0_var(--primary)]"
                          : "[&>td]:shadow-[inset_0_2px_0_0_var(--primary)]"),
                    )}
                  >
                    <TableCell className="w-px">
                      {/* Only the handle is draggable, so pressing the switch or
                          the name can't turn into a drag. The buttons in the
                          actions cell do the same job without a pointer. */}
                      <span
                        draggable={!disabled}
                        aria-hidden="true"
                        title={`Drag to reorder ${label}`}
                        onDragStart={(event) => {
                          setDrag(index)
                          event.dataTransfer.effectAllowed = "move"
                          event.dataTransfer.setData("text/plain", String(index))
                        }}
                        onDragEnd={endDrag}
                        className={cn(
                          "flex size-5 items-center justify-center rounded text-muted-foreground/60 transition-colors",
                          disabled
                            ? "cursor-default"
                            : "cursor-grab hover:text-foreground active:cursor-grabbing",
                        )}
                      >
                        <GripVertical className="size-3.5" />
                      </span>
                    </TableCell>
                    <TableCell className="w-px">
                      {/* A bare inline-block switch sits on the cell's text
                          baseline, which leaves it a few px above the row's
                          optical center. A flex wrapper centers it. */}
                      <div className="flex items-center">
                        <Switch
                          size="sm"
                          aria-label={`Turn ${label} ${entry.enabled ? "off" : "on"}`}
                          checked={entry.enabled}
                          disabled={disabled}
                          onCheckedChange={(enabled) => set(index, { ...entry, enabled })}
                        />
                      </div>
                    </TableCell>
                    <TableCell className="max-w-0">
                      <button
                        type="button"
                        disabled={disabled}
                        onClick={() => openEditor({ index, entry })}
                        className="flex max-w-full items-center gap-1.5 text-left font-medium hover:underline disabled:pointer-events-none"
                      >
                        {invalid ? (
                          <AlertCircle className="size-3.5 shrink-0 text-destructive" aria-label="Needs fixing" />
                        ) : null}
                        <span className={entry.enabled ? "truncate font-mono" : "truncate font-mono text-muted-foreground"}>
                          {label}
                        </span>
                      </button>
                      <div className="truncate text-xs text-muted-foreground sm:hidden">
                        <RunsIn entry={entry} candidates={candidates} loaded={loaded} />
                      </div>
                    </TableCell>
                    <TableCell
                      className="hidden max-w-0 truncate text-muted-foreground sm:table-cell"
                      title={runsInText(entry, candidates, loaded)}
                    >
                      <RunsIn entry={entry} candidates={candidates} loaded={loaded} />
                    </TableCell>
                    <TableCell align="end" className="w-px">
                      {/* Below sm four buttons would take half the row and
                          squeeze the name to an ellipsis; they fold into a menu. */}
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-xs"
                            className="sm:hidden"
                            aria-label={`More actions for ${label}`}
                            disabled={disabled}
                          >
                            <MoreHorizontal className="size-4" />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuItem onSelect={() => openEditor({ index, entry })}>
                            <Pencil className="size-4" />
                            Configure
                          </DropdownMenuItem>
                          <DropdownMenuItem disabled={index === 0} onSelect={() => move(index, -1)}>
                            <ArrowUp className="size-4" />
                            Move up
                          </DropdownMenuItem>
                          <DropdownMenuItem disabled={index === list.length - 1} onSelect={() => move(index, 1)}>
                            <ArrowDown className="size-4" />
                            Move down
                          </DropdownMenuItem>
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            variant="destructive"
                            onSelect={() => onChange({ environments: list.filter((_, i) => i !== index) })}
                          >
                            <Trash2 className="size-4" />
                            Remove
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                      <TableRowActions className="hidden sm:inline-flex">
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
                          aria-label={`Configure ${label}`}
                          disabled={disabled}
                          onClick={() => openEditor({ index, entry })}
                        >
                          <Pencil className="size-4" />
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
                      </TableRowActions>
                    </TableCell>
                  </MotionRow>
                )
              })}
            </AnimatePresence>
          )}
        </TableBody>
      </Table>

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
          onClick={addEnvironment}
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
              openEditor({
                index: null,
                entry: { ...SCRATCH, name: taken.includes("scratch") ? nameFor("scratch", taken) : "scratch" },
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

      <Dialog open={editing !== null} onOpenChange={(open) => (open ? undefined : setEditing(null))}>
        <DialogContent className="sm:max-w-md">
          {currentEditing ? (
            <EntryEditor
              // A fresh copy each time the dialog opens on a different row.
              key={editingFormKey}
              initial={currentEditing.entry}
              adding={currentEditing.index === null}
              // The plugin's refusals belong to the stored row, not one not added yet.
              errors={
                currentEditing.index === null ? {} : entryErrors(errors, currentEditing.index)
              }
              taken={list
                .filter((_, i) => i !== currentEditing.index)
                .map((entry) => entry.name)}
              candidates={candidates}
              loaded={loaded}
              onApply={apply}
              onCancel={() => setEditing(null)}
            />
          ) : null}
        </DialogContent>
      </Dialog>
    </div>
  )
}

function runsInText(entry: Entry, candidates: EnvironmentCandidate[], loaded: boolean): string {
  if (entry.type === "scratch") return "Scratch sandbox"
  if (!entry.environmentId) return "Not picked"
  const candidate = candidates.find((c) => c.id === entry.environmentId)
  if (!candidate) return loaded ? "Unknown environment" : "Loading…"
  return describe(candidate)
}

/** Where an entry's commands run, as the table's second column says it. */
function RunsIn({
  entry,
  candidates,
  loaded,
}: {
  entry: Entry
  candidates: EnvironmentCandidate[]
  loaded: boolean
}) {
  const said = runsInText(entry, candidates, loaded)
  return entry.type === "user" && !entry.environmentId ? <span className="text-destructive">{said}</span> : <>{said}</>
}

function EntryEditor({
  initial,
  adding,
  errors,
  taken,
  candidates,
  loaded,
  onApply,
  onCancel,
}: {
  initial: Entry
  adding: boolean
  errors: EntryErrors
  taken: string[]
  candidates: EnvironmentCandidate[]
  loaded: boolean
  onApply: (entry: Entry) => void
  onCancel: () => void
}) {
  const [entry, setEntry] = React.useState(initial)
  const trimmed = entry.name.trim()
  // Caught here so the row isn't applied only to be refused on Save; every
  // other rule is still the plugin's.
  const clash = trimmed && taken.includes(trimmed) ? "Another environment already has this name." : undefined
  const nameError = clash ?? (entry.name === initial.name ? errors.name : undefined)
  const pickError =
    entry.environmentId === initial.environmentId ? errors.environmentId : undefined
  const canApply = trimmed !== "" && !clash && (entry.type === "scratch" || entry.environmentId !== "")

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault()
    // React bubbles events through portals: without this the settings
    // dialog's own form would take the submit and save.
    event.stopPropagation()
    if (canApply) onApply(entry)
  }

  const title = adding
    ? entry.type === "scratch"
      ? "Add scratch"
      : "Add environment"
    : `Configure ${initial.name || (entry.type === "scratch" ? "scratch" : "environment")}`

  return (
    <form onSubmit={handleSubmit} className="flex flex-col gap-4">
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>
          {entry.type === "scratch"
            ? "A sandbox Faber provides to each session. Its files last as long as the session."
            : "One of your registered hosts or containers, under the name the agent uses for it."}
        </DialogDescription>
      </DialogHeader>

      <div className="flex flex-col gap-4">
        <Field
          id="env-edit-name"
          label="Name"
          value={entry.name}
          hint="What the agent calls it."
          error={nameError}
          autoFocus
          onChange={(name) => setEntry({ ...entry, name })}
        />

        {entry.type === "user" ? (
          <div className="w-full">
            <label htmlFor="env-edit-environment" className="mb-1.5 block text-sm font-medium text-foreground/80">
              Environment
            </label>
            <Select
              value={entry.environmentId}
              onValueChange={(environmentId) => setEntry({ ...entry, environmentId })}
            >
              <SelectTrigger id="env-edit-environment" aria-invalid={Boolean(pickError) || undefined}>
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
            <FieldError message={pickError} />
          </div>
        ) : null}

        <div className="flex items-center justify-between gap-3">
          <label htmlFor="env-edit-enabled" className="text-sm font-medium text-foreground/80">
            Enabled
          </label>
          <Switch
            id="env-edit-enabled"
            checked={entry.enabled}
            onCheckedChange={(enabled) => setEntry({ ...entry, enabled })}
          />
        </div>

        <FieldError message={errors.type ?? errors.id ?? errors.enabled} />
      </div>

      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" disabled={!canApply}>
          {adding ? "Add" : "Apply"}
        </Button>
      </DialogFooter>
    </form>
  )
}

function Field({
  error,
  ...props
}: React.ComponentProps<typeof Input> & { error?: string }) {
  return (
    <div className="w-full">
      <Input {...props} hint={error ? undefined : props.hint} />
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
