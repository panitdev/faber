import { Bot, MoreHorizontal, Pencil, Plus, Trash2 } from "lucide-react"
import * as React from "react"

import {
  faber,
  FaberError,
  type AgentDefinition,
  type AgentDefinitionField,
  type ModelConfig,
  type PluginType,
  type Project,
} from "@/lib/api"
import { Button } from "@charlotte-ui/button"
import { Input } from "@charlotte-ui/input"
import { Switch } from "@charlotte-ui/switch"
import { Textarea } from "@charlotte-ui/textarea"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@charlotte-ui/select"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
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
  DropdownMenuTrigger,
} from "@charlotte-ui/dropdown-menu"

/** The spawning session's model, as the picker offers it. */
const PARENTS_MODEL = "__parent__"

interface Draft {
  name: string
  description: string
  prompt: string
  model: string
  /** `null`: the spawning session's plugins. */
  plugins: string[] | null
}

function draftOf(definition: AgentDefinition | null): Draft {
  return {
    name: definition?.name ?? "",
    description: definition?.description ?? "",
    prompt: definition?.prompt ?? "",
    model: definition?.model ?? PARENTS_MODEL,
    plugins: definition?.plugins ?? null,
  }
}

type FieldErrors = Partial<Record<AgentDefinitionField, string>>

/** The field a refusal is about, from the `errors` the API sent with it. */
function fieldOf(err: FaberError): AgentDefinitionField | null {
  const field = (err.details as { field?: unknown } | undefined)?.field
  return field === "name" || field === "description" || field === "prompt" || field === "model" || field === "plugins"
    ? field
    : null
}

function FieldError({ id, message }: { id: string; message: string | undefined }) {
  if (!message) return null
  return (
    <p id={id} role="alert" className="text-xs text-destructive">
      {message}
    </p>
  )
}

/** Creates a definition, or edits `definition`. */
function DefinitionForm({
  project,
  definition,
  types,
  onSaved,
}: {
  project: Project
  definition: AgentDefinition | null
  types: PluginType[]
  onSaved: (saved: AgentDefinition) => void
}) {
  const [draft, setDraft] = React.useState(() => draftOf(definition))
  const [models, setModels] = React.useState<ModelConfig[] | null>(null)
  const [errors, setErrors] = React.useState<FieldErrors>({})
  const [error, setError] = React.useState<string | null>(null)
  const [saving, setSaving] = React.useState(false)

  React.useEffect(() => {
    void faber.listModels().then(setModels, () => setModels([]))
  }, [])

  const nameOf = (type: string) => types.find((found) => found.id === type)?.name ?? type
  const limited = draft.plugins !== null
  const set = (patch: Partial<Draft>) => setDraft((current) => ({ ...current, ...patch }))

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSaving(true)
    setErrors({})
    setError(null)
    const body = {
      name: draft.name.trim(),
      description: draft.description.trim(),
      prompt: draft.prompt.trim(),
      model: draft.model === PARENTS_MODEL ? null : draft.model,
      plugins: draft.plugins,
    }
    try {
      const saved = definition
        ? await faber.updateAgentDefinition(project.id, definition.name, body)
        : await faber.createAgentDefinition(project.id, body)
      onSaved(saved)
    } catch (err) {
      const field = err instanceof FaberError ? fieldOf(err) : null
      const message = err instanceof FaberError ? err.message : "the change didn't go through"
      if (field) setErrors({ [field]: message })
      else setError(message)
    } finally {
      setSaving(false)
    }
  }

  return (
    <form onSubmit={handleSubmit} className="flex min-h-0 flex-col gap-5">
      <DialogHeader>
        <DialogTitle>{definition ? `Edit ${definition.name}` : "New agent"}</DialogTitle>
        <DialogDescription>
          A kind of agent this project&apos;s threads can start. The agent reads its description to
          decide when to.
        </DialogDescription>
      </DialogHeader>

      <div className="flex flex-col gap-1.5">
        <Input
          id="agent-name"
          label="Name"
          placeholder="reviewer"
          value={draft.name}
          disabled={saving}
          autoFocus
          hint="Lower-case letters, digits and hyphens."
          onChange={(name) => set({ name })}
        />
        <FieldError id="agent-name-error" message={errors.name} />
      </div>

      <div className="flex flex-col gap-1.5">
        <label htmlFor="agent-description" className="text-sm">
          Description
        </label>
        <Textarea
          id="agent-description"
          rows={2}
          placeholder="Reviews a change for bugs, and reports what it found."
          value={draft.description}
          disabled={saving}
          aria-invalid={!!errors.description}
          aria-describedby={errors.description ? "agent-description-error" : undefined}
          onChange={(event) => set({ description: event.target.value })}
        />
        <FieldError id="agent-description-error" message={errors.description} />
      </div>

      <div className="flex flex-col gap-1.5">
        <label htmlFor="agent-prompt" className="text-sm">
          Instructions
        </label>
        <Textarea
          id="agent-prompt"
          rows={5}
          placeholder="Added to the start of every agent started as this one."
          value={draft.prompt}
          disabled={saving}
          aria-invalid={!!errors.prompt}
          aria-describedby={errors.prompt ? "agent-prompt-error" : undefined}
          onChange={(event) => set({ prompt: event.target.value })}
        />
        <FieldError id="agent-prompt-error" message={errors.prompt} />
      </div>

      <div className="flex flex-col gap-1.5">
        <label htmlFor="agent-model" className="text-sm">
          Model
        </label>
        <Select value={draft.model} onValueChange={(model) => set({ model })}>
          <SelectTrigger id="agent-model" aria-invalid={!!errors.model}>
            <SelectValue placeholder={models ? "Pick a model" : "Loading…"} />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={PARENTS_MODEL}>The thread&apos;s own model</SelectItem>
            {(models ?? []).map((model) => (
              <SelectItem key={model.id} value={model.alias}>
                {model.alias}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <FieldError id="agent-model-error" message={errors.model} />
      </div>

      <section className="flex flex-col gap-2">
        <div className="flex items-center gap-3">
          <span className="flex-1 text-sm">Limit which plugins it can use</span>
          <Switch
            size="sm"
            checked={limited}
            onCheckedChange={(on) => set({ plugins: on ? [] : null })}
            aria-label="Limit which plugins it can use"
          />
        </div>
        {limited ? (
          project.bindings.length === 0 ? (
            <p className="text-xs text-muted-foreground">This project has no plugins.</p>
          ) : (
            <ul>
              {project.bindings.map((binding) => {
                const allowed = draft.plugins?.includes(binding.type) ?? false
                return (
                  <li
                    key={binding.type}
                    className="flex h-9 items-center gap-3 border-t border-border/60 text-sm"
                  >
                    <span className="flex-1">{nameOf(binding.type)}</span>
                    <Switch
                      size="sm"
                      checked={allowed}
                      onCheckedChange={(on) =>
                        set({
                          plugins: on
                            ? [...(draft.plugins ?? []), binding.type]
                            : (draft.plugins ?? []).filter((type) => type !== binding.type),
                        })
                      }
                      aria-label={`Allow ${nameOf(binding.type)}`}
                    />
                  </li>
                )
              })}
            </ul>
          )
        ) : (
          <p className="text-xs text-muted-foreground">It gets the plugins of the thread that starts it.</p>
        )}
        <FieldError id="agent-plugins-error" message={errors.plugins} />
      </section>

      {error ? <p className="text-sm text-destructive">{error}</p> : null}

      <DialogFooter>
        <Button type="submit" loading={saving} loadingText="Saving">
          {definition ? "Save" : "Create"}
        </Button>
      </DialogFooter>
    </form>
  )
}

/**
 * The kinds of agent a project's threads can start: the built-in one, then
 * the owner's own, each made, edited and deleted here.
 */
export function AgentDefinitionsSection({ project }: { project: Project }) {
  const [definitions, setDefinitions] = React.useState<AgentDefinition[] | null>(null)
  const [types, setTypes] = React.useState<PluginType[]>([])
  React.useEffect(() => {
    // Only for the plugins' names; their ids do without.
    void faber.listPluginTypes().then(setTypes, () => undefined)
  }, [])
  const [error, setError] = React.useState<string | null>(null)
  /** `undefined`: closed; `null`: a new one. */
  const [editing, setEditing] = React.useState<AgentDefinition | null | undefined>(undefined)
  const [deleting, setDeleting] = React.useState<AgentDefinition | null>(null)

  const load = React.useCallback(() => {
    void faber.listAgentDefinitions(project.id).then(setDefinitions, (err) =>
      setError(err instanceof FaberError ? err.message : "failed to load the agents"),
    )
  }, [project.id])
  React.useEffect(load, [load])

  const remove = async (definition: AgentDefinition) => {
    setError(null)
    try {
      await faber.deleteAgentDefinition(project.id, definition.name)
      load()
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to delete the agent")
    }
  }

  return (
    <section aria-label="Agents" className="flex flex-col">
      <h2 className="pb-2 text-sm text-muted-foreground">Agents</h2>
      <ul>
        {definitions === null ? (
          <li className="border-t border-border/60 py-2.5 text-sm text-muted-foreground">Loading…</li>
        ) : (
          definitions.map((definition) => (
            <li
              key={definition.name}
              data-definition={definition.name}
              className="group/agent flex min-h-10 items-center gap-3 border-t border-border/60 py-1.5"
            >
              <Bot className="size-4 shrink-0 text-muted-foreground" />
              <div className="flex min-w-0 flex-1 flex-col">
                <span className="truncate text-[15px]">{definition.name}</span>
                <span className="truncate text-xs text-muted-foreground">
                  {definition.builtin ? "Built in · " : ""}
                  {definition.model ? `${definition.model} · ` : ""}
                  {definition.description}
                </span>
              </div>
              {definition.builtin ? null : (
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      aria-label={`More actions for ${definition.name}`}
                      className="size-7 opacity-0 group-hover/agent:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
                    >
                      <MoreHorizontal className="size-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setEditing(definition)}>
                      <Pencil className="h-4 w-4" />
                      Edit
                    </DropdownMenuItem>
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleting(definition)}>
                      <Trash2 className="h-4 w-4" />
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              )}
            </li>
          ))
        )}
        <li className="border-t border-border/60">
          <button
            type="button"
            onClick={() => setEditing(null)}
            className="flex h-10 w-full items-center gap-3 text-left text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground"
          >
            <Plus className="size-4 shrink-0" />
            New agent
          </button>
        </li>
      </ul>
      {error ? <p className="pt-2 text-sm text-destructive">{error}</p> : null}
      <p className="pt-3 text-xs text-muted-foreground">Threads see changes from their next run.</p>

      <Dialog open={editing !== undefined} onOpenChange={(open) => !open && setEditing(undefined)}>
        <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-xl">
          {editing !== undefined ? (
            <DefinitionForm
              key={editing?.name ?? "new"}
              project={project}
              definition={editing}
              types={types}
              onSaved={() => {
                setEditing(undefined)
                load()
              }}
            />
          ) : null}
        </DialogContent>
      </Dialog>

      <AlertDialog open={!!deleting} onOpenChange={(open) => !open && setDeleting(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleting?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Threads can no longer start it. Agents already started as it carry on.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (deleting) void remove(deleting)
                setDeleting(null)
              }}
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}
