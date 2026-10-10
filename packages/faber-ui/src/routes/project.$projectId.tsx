import * as React from "react"
import { createFileRoute, useNavigate } from "@tanstack/react-router"
import {
  ChevronDown,
  MoreHorizontal,
  Pencil,
  Plus,
  Puzzle,
  Settings2,
  Trash2,
  Unplug,
} from "lucide-react"

import {
  faber,
  FaberError,
  type JsonValue,
  type PluginType,
  type Project,
  type ProjectBinding,
  type Uuid,
} from "@/lib/api"
import { cn } from "@/lib/utils"
import { sessionLabel, timeAgo } from "@/lib/sessions/labels"
import { useAppShell } from "@/components/shell/app-shell"
import { DeleteProjectDialog, EditProjectDialog } from "@/components/shell/project-dialogs"
import {
  hasSettings,
  PluginSettingsDialog,
  type SettingsTarget,
} from "@/components/plugins/plugin-settings"
import { PromptBox } from "@/components/thread/prompt-box"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"

/**
 * One project: where its threads start, what they are, and the plugins they
 * run through.
 *
 * A thread started here reaches environments through the project's plugins
 * rather than through `@` tags, so the composer offers none.
 */
export const Route = createFileRoute("/project/$projectId")({ component: ProjectRoute })

function ProjectRoute() {
  const { projectId } = Route.useParams()
  // Keyed so moving between projects starts each page from its own defaults.
  return <ProjectPage key={projectId} projectId={projectId as Uuid} />
}

function ProjectPage({ projectId }: { projectId: Uuid }) {
  const navigate = useNavigate()
  const {
    projects,
    projectsLoading,
    sessions,
    models,
    modelsLoaded,
    selectedModel,
    selectModel,
    selectedThinking,
    selectThinking,
    createSession,
    deleteProject,
    storeProject,
  } = useAppShell()

  const project = projects.find((row) => row.id === projectId) ?? null
  const threads = sessions.filter((session) => session.project_id === projectId)

  // The shell's copy may predate an edit made in another tab; a fresh read
  // gives this page the `rev` its writes will be checked against.
  const [missing, setMissing] = React.useState(false)
  React.useEffect(() => {
    let cancelled = false
    void faber
      .getProject(projectId)
      .then((found) => {
        if (!cancelled) storeProject(found)
      })
      .catch((err) => {
        if (!cancelled && err instanceof FaberError && err.isNotFound) setMissing(true)
      })
    return () => {
      cancelled = true
    }
  }, [projectId, storeProject])

  const [editing, setEditing] = React.useState(false)
  const [deleting, setDeleting] = React.useState(false)

  const [sendError, setSendError] = React.useState<string | null>(null)
  const [sending, setSending] = React.useState(false)
  const noModels = modelsLoaded && models.length === 0

  const handleSend = React.useCallback(
    async (content: string) => {
      setSendError(null)
      const model = selectedModel?.alias
      if (!model) {
        setSendError("Add a model before starting a thread.")
        return false
      }
      setSending(true)
      try {
        const created = await createSession(projectId)
        if (!created) return false
        await faber.sendMessage(created.id, {
          content,
          model,
          ...(selectedThinking ? { thinking_effort: selectedThinking } : {}),
        })
        navigate({ to: "/session/$sessionId", params: { sessionId: created.id } })
        return true
      } catch (err) {
        setSendError(err instanceof FaberError ? err.message : "failed to send the message")
        return false
      } finally {
        setSending(false)
      }
    },
    [projectId, selectedModel, selectedThinking, createSession, navigate],
  )

  const saveProject = React.useCallback(
    async (id: Uuid, patch: { name: string; description: string }) => {
      if (!project) throw new FaberError(404, "project not found")
      const updated = await faber.updateProject(id, project.rev, patch)
      storeProject(updated)
      return updated
    },
    [project, storeProject],
  )

  if (!project) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-4">
        <p className="text-sm text-muted-foreground">
          {missing || !projectsLoading ? "This project doesn't exist, or it isn't yours." : "Loading…"}
        </p>
      </div>
    )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* The trail and the project's own menu. Below `md` the top bar already
          names the project, so this row would only repeat it. */}
      <div className="hidden h-12 shrink-0 items-center gap-1.5 px-6 text-sm md:flex">
        <span className="text-muted-foreground">Projects</span>
        <span aria-hidden className="text-muted-foreground/60">/</span>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="-mx-1.5 flex min-w-0 items-center gap-1 rounded-md px-1.5 py-1 font-medium outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring/50"
            >
              <span className="truncate">{project.name}</span>
              <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuItem onSelect={() => setEditing(true)}>
              <Pencil className="h-4 w-4" />
              Edit details
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onSelect={() => setDeleting(true)}>
              <Trash2 className="h-4 w-4" />
              Delete project
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto grid w-full max-w-6xl gap-10 px-4 py-6 md:px-8 md:py-8 lg:grid-cols-[minmax(0,1fr)_18rem] lg:gap-x-14 lg:gap-y-8">
          {/* Above both columns, so the composer and the rail start level. */}
          <div className="flex flex-col gap-1.5 lg:col-span-2">
            <h1 className="text-3xl font-semibold tracking-tight break-words">{project.name}</h1>
            {project.description ? (
              <p className="text-sm whitespace-pre-line text-muted-foreground">
                {project.description}
              </p>
            ) : null}
          </div>

          <div className="flex min-w-0 flex-col gap-8">

            <div className="flex flex-col gap-2">
              <PromptBox
                className="w-full"
                autoFocus
                placeholder={
                  noModels ? "Add a model to start chatting…" : `New thread in ${project.name}`
                }
                sendDisabled={noModels || sending}
                onSend={handleSend}
                models={models}
                selectedModel={selectedModel}
                onModelSelect={selectModel}
                modelsLoaded={modelsLoaded}
                selectedThinking={selectedThinking}
                onThinkingSelect={selectThinking}
                // The composer sits near the top of this page, so the menu
                // has room below it and none above.
                menuSide="bottom"
              />
              {sendError ? <p className="text-sm text-destructive">{sendError}</p> : null}
            </div>

            <section className="flex flex-col">
              <h2 className="pb-2 text-sm text-muted-foreground">Recents</h2>
              {threads.length === 0 ? (
                <p className="border-t border-border/60 py-4 text-sm text-muted-foreground">
                  No threads yet. Write above to start the first one.
                </p>
              ) : (
                <ul>
                  {threads.map((session) => (
                    <li key={session.id} className="border-t border-border/60">
                      <button
                        type="button"
                        onClick={() =>
                          navigate({ to: "/session/$sessionId", params: { sessionId: session.id } })
                        }
                        className="-mx-2 flex w-[calc(100%+1rem)] items-center gap-4 rounded-md px-2 py-3.5 text-left outline-none hover:bg-accent/60 focus-visible:ring-2 focus-visible:ring-ring/50"
                      >
                        <span className="min-w-0 flex-1 truncate text-[15px]">
                          {sessionLabel(session)}
                        </span>
                        <span className="shrink-0 text-sm text-muted-foreground">
                          {timeAgo(session.created_at)}
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          </div>

          <aside className="flex min-w-0 flex-col">
            <PluginsSection project={project} onChange={storeProject} />
          </aside>
        </div>
      </div>

      <EditProjectDialog
        key={editing ? `edit:${project.id}` : "none"}
        project={editing ? project : null}
        onOpenChange={setEditing}
        onSave={saveProject}
      />

      <DeleteProjectDialog
        project={deleting ? project : null}
        threadCount={threads.length}
        onOpenChange={setDeleting}
        onDelete={deleteProject}
      />
    </div>
  )
}

/**
 * The project's bindings, in the order the model sees their tools. Binding and
 * removing are made against the project's `rev`; a stale one reloads the
 * project rather than retrying, so the user sees what changed before deciding
 * again. Toggles carry no `rev`: each sets an absolute value, so the user's
 * last click is the answer.
 */
function PluginsSection({
  project,
  onChange,
}: {
  project: Project
  onChange: (project: Project) => void
}) {
  const [types, setTypes] = React.useState<PluginType[] | null>(null)
  const [busy, setBusy] = React.useState<string | null>(null)
  const [error, setError] = React.useState<string | null>(null)
  const [unbindTarget, setUnbindTarget] = React.useState<ProjectBinding | null>(null)
  const [settingsTarget, setSettingsTarget] = React.useState<SettingsTarget | null>(null)

  React.useEffect(() => {
    let cancelled = false
    void faber
      .listPluginTypes()
      .then((found) => {
        if (!cancelled) setTypes(found)
      })
      .catch(() => {
        if (!cancelled) setTypes([])
      })
    return () => {
      cancelled = true
    }
  }, [])

  const typeOf = (id: string) => types?.find((type) => type.id === id)
  const unbound = (types ?? []).filter(
    (type) => !project.bindings.some((binding) => binding.type === type.id),
  )

  // The newest project and the toggles still on the wire, read by writes that
  // start after an await.
  const projectRef = React.useRef(project)
  projectRef.current = project
  const sending = React.useRef(new Map<string, Promise<void>>())

  // Bind and unbind are checked against `rev`, so they wait for toggles
  // already sent: each one bumps it, and starting earlier would only conflict
  // with the user's own clicks.
  const run = async (key: string, write: (rev: number) => Promise<Project>) => {
    setBusy(key)
    setError(null)
    try {
      await Promise.all(sending.current.values())
      onChange(await write(projectRef.current.rev))
    } catch (err) {
      if (err instanceof FaberError && err.code === "rev-mismatch") {
        setError("This project changed somewhere else. It has been reloaded; try again.")
        void faber.getProject(project.id).then(onChange, () => undefined)
      } else {
        setError(err instanceof FaberError ? err.message : "the change didn't go through")
      }
    } finally {
      setBusy(null)
    }
  }

  // What each switch shows until the server has caught up with it. A switch
  // has at most one write on the wire, so requests for it can't land out of
  // order; clicks made meanwhile are sent once it answers, last one only.
  const [desired, setDesired] = React.useState<Record<string, boolean>>({})
  const desiredRef = React.useRef(desired)
  const settle = (type: string) => {
    const rest = { ...desiredRef.current }
    delete rest[type]
    desiredRef.current = rest
    setDesired(rest)
  }

  const send = async (type: string) => {
    try {
      let sent: boolean
      do {
        sent = desiredRef.current[type]
        onChange(await faber.setPluginEnabled(project.id, type, sent))
      } while (desiredRef.current[type] !== sent)
      settle(type)
    } catch (err) {
      settle(type)
      setError(err instanceof FaberError ? err.message : "the change didn't go through")
      void faber.getProject(project.id).then(onChange, () => undefined)
    }
  }

  const toggle = (binding: ProjectBinding, enabled: boolean) => {
    setError(null)
    desiredRef.current = { ...desiredRef.current, [binding.type]: enabled }
    setDesired(desiredRef.current)
    if (sending.current.has(binding.type)) return
    const done = send(binding.type).finally(() => sending.current.delete(binding.type))
    sending.current.set(binding.type, done)
  }
  const unbind = (binding: ProjectBinding) =>
    run(binding.type, (rev) => faber.unbindPlugin(project.id, rev, binding.type))
  const bind = (type: PluginType) =>
    run(type.id, (rev) => faber.bindPlugin(project.id, rev, type.id))

  // The dialog shows the refusal on its own fields, so this throws rather
  // than going through `run`. A stale `rev` still reloads the project, and
  // the user saves again over what changed.
  const saveSettings = async (type: string, config: JsonValue) => {
    await Promise.all(sending.current.values())
    try {
      onChange(await faber.bindPlugin(project.id, projectRef.current.rev, type, { config }))
    } catch (err) {
      if (err instanceof FaberError && err.code === "rev-mismatch") {
        void faber.getProject(project.id).then(onChange, () => undefined)
        throw new FaberError(
          err.status,
          "This project changed somewhere else. It has been reloaded; save again.",
          err.code,
        )
      }
      throw err
    }
  }

  return (
    <section className="flex flex-col">
      <h2 className="pb-2 text-sm text-muted-foreground">Plugins</h2>
      <ul>
        {types === null ? (
          <li className="border-t border-border/60 py-2.5 text-sm text-muted-foreground">Loading…</li>
        ) : project.bindings.length === 0 ? (
          <li className="border-t border-border/60 py-2.5 text-sm text-muted-foreground">
            No plugins. Threads here get no tools beyond the model.
          </li>
        ) : (
          project.bindings.map((binding) => {
            const type = typeOf(binding.type)
            const name = type?.name ?? binding.type
            const enabled = desired[binding.type] ?? binding.enabled
            return (
              <li
                key={binding.type}
                className="group/plugin flex h-10 items-center gap-3 border-t border-border/60"
              >
                <Puzzle className="size-4 shrink-0 text-muted-foreground" />
                {/* Name only: a description helps when choosing a plugin, and
                    that happens in the "Add plugin" picker. */}
                <span
                  className={cn(
                    "min-w-0 flex-1 truncate text-[15px]",
                    !enabled && "text-muted-foreground",
                  )}
                >
                  {name}
                </span>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      aria-label={`More actions for ${name}`}
                      disabled={busy !== null}
                      className="size-7 opacity-0 group-hover/plugin:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
                    >
                      <MoreHorizontal className="size-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuLabel className="font-normal text-muted-foreground">
                      Version {binding.version}
                    </DropdownMenuLabel>
                    {hasSettings(binding.type) ? (
                      <DropdownMenuItem
                        onSelect={() =>
                          setSettingsTarget({ type: binding.type, name, config: binding.config })
                        }
                      >
                        <Settings2 className="h-4 w-4" />
                        Settings
                      </DropdownMenuItem>
                    ) : null}
                    <DropdownMenuItem variant="destructive" onSelect={() => setUnbindTarget(binding)}>
                      <Unplug className="h-4 w-4" />
                      Remove
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
                {/* Only a removal of this binding locks it: toggles never wait,
                    so nothing dims while one is on the wire. */}
                <Switch
                  size="sm"
                  checked={enabled}
                  disabled={busy === binding.type}
                  onCheckedChange={(next) => toggle(binding, next)}
                  aria-label={`${enabled ? "Disable" : "Enable"} ${name}`}
                />
              </li>
            )
          })
        )}
        {unbound.length > 0 ? (
          <li className="border-t border-border/60">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  disabled={busy !== null}
                  className="flex h-10 w-full items-center gap-3 text-left text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground disabled:opacity-50"
                >
                  <Plus className="size-4 shrink-0" />
                  Add plugin
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="max-w-xs">
                {unbound.map((type) => (
                  <DropdownMenuItem key={type.id} onSelect={() => void bind(type)}>
                    <Puzzle className="h-4 w-4" />
                    <div className="flex min-w-0 flex-col">
                      <span>{type.name}</span>
                      <span className="truncate text-xs text-muted-foreground">
                        {type.description}
                      </span>
                    </div>
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          </li>
        ) : null}
      </ul>

      {error ? <p className="pt-2 text-sm text-destructive">{error}</p> : null}
      <p className="pt-3 text-xs text-muted-foreground">
        Changes reach each thread at its next run.
      </p>

      <PluginSettingsDialog
        key={settingsTarget?.type ?? "none"}
        target={settingsTarget}
        onOpenChange={(open) => !open && setSettingsTarget(null)}
        onSave={saveSettings}
      />

      <AlertDialog open={!!unbindTarget} onOpenChange={(open) => !open && setUnbindTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Remove {unbindTarget ? (typeOf(unbindTarget.type)?.name ?? unbindTarget.type) : "plugin"}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              Its settings for this project are deleted with it, and threads here lose its tools
              at their next run. Turning it off instead keeps the settings.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (unbindTarget) void unbind(unbindTarget)
                setUnbindTarget(null)
              }}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              Remove
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}
