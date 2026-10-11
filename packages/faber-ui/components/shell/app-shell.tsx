import * as React from "react"
import { useLocation, useNavigate } from "@tanstack/react-router"

import {
  faber,
  FaberError,
  type CreateModelRequest,
  type CreatedSession,
  type FaberConfig,
  type Me,
  type ModelConfig,
  type Project,
  type Session,
  type ThinkingSelection,
  type UpdateModelRequest,
  type Uuid,
} from "@/lib/api"
import { projectNavKey, sessionNavKey } from "@/lib/sessions/labels"
import type { SessionSelection } from "@/lib/sessions/use-session-selection"
import type { DebugView } from "@/components/thread/debug-viewer"
import { AppSidebar } from "@/components/shell/app-sidebar"
import { GlobalCommandDialog } from "@/components/shell/global-command-dialog"
import { MobileTopBar } from "@/components/shell/mobile-nav"

type AppShellContextValue = {
  config: FaberConfig | null
  /** The signed-in user, or `null` until it arrives. */
  me: Me | null
  sessions: Session[]
  sessionsLoading: boolean
  /** Most recently changed first, as the API lists them. */
  projects: Project[]
  projectsLoading: boolean
  models: ModelConfig[]
  modelsLoaded: boolean
  /**
   * The draft model a *new* thread starts on — the user's pick, or the first
   * model. A thread that exists carries its own, persisted on the session;
   * this is only what the landing page opens one with.
   */
  selectedModel: ModelConfig | null
  selectModel: (alias: string) => void
  /** The draft thinking knob, for the same window and the same reason. */
  selectedThinking: ThinkingSelection | null
  selectThinking: (selection: ThinkingSelection | null) => void
  creatingSession: boolean
  createError: string | null
  /** A workspace-only thread, or one in `projectId` when given. */
  createSession: (projectId?: Uuid | null) => Promise<CreatedSession | null>
  renameSession: (id: Uuid, title: string) => Promise<Session>
  updateSessionTitle: (id: Uuid, title: string) => void
  deleteSession: (id: Uuid) => Promise<void>
  createProject: (name: string, description?: string) => Promise<Project>
  renameProject: (id: Uuid, name: string) => Promise<Project>
  /** Also drops the project's sessions, which the server deletes with it. */
  deleteProject: (id: Uuid) => Promise<void>
  /**
   * Takes a project the API just returned — from a page that edited its
   * bindings or description — so the sidebar and the next `If-Match` see the
   * new `rev`.
   */
  storeProject: (project: Project) => void
  addModel: (body: CreateModelRequest) => Promise<ModelConfig>
  editModel: (id: Uuid, patch: UpdateModelRequest) => Promise<ModelConfig>
  removeModel: (id: Uuid) => Promise<void>
  /**
   * The open thread's own model and thinking selection, registered by
   * `SessionThread` while it is mounted. `null` on every other page, where the
   * draft above is the selection in effect.
   */
  threadSelection: SessionSelection | null
  setThreadSelection: (selection: SessionSelection | null) => void
  /**
   * Which raw log the debug viewer is showing, or `null` when it is closed.
   * App-wide rather than local to the palette so the overlay can be rendered
   * inside the conversation viewport instead of over the whole frame.
   */
  debugViewer: DebugView | null
  setDebugViewer: (view: DebugView | null) => void
}

const AppShellContext = React.createContext<AppShellContextValue | null>(null)

/** Sessions, models, and thread creation — shared by the landing page and every session page. */
export function useAppShell(): AppShellContextValue {
  const ctx = React.useContext(AppShellContext)
  if (!ctx) throw new Error("useAppShell must be used within <AppShell>")
  return ctx
}

const SESSION_PATH_PREFIX = "/session/"
const PROJECT_PATH_PREFIX = "/project/"

/** Static routes the sidebar highlights. Sessions key off their id instead. */
const NAV_KEY_BY_PATH: Record<string, string> = {
  "/models": "models",
  "/credentials": "credentials",
  "/environments": "environments",
}

/**
 * The app frame: sidebar plus the session/model state it and every page under
 * it need. Lives in the root layout so it persists across navigations between
 * threads — the sidebar's list and scroll position survive a route change.
 */
export function AppShell({ children }: { children: React.ReactNode }) {
  const navigate = useNavigate()
  // Old links and bookmarks may carry a trailing slash — normalize so the
  // sidebar highlights the same row regardless, rather than losing the
  // indicator on refresh and direct links.
  const pathname = (useLocation().pathname ?? "").replace(/\/+$/, "") || "/"
  const activeNavKey: string | null = pathname.startsWith(SESSION_PATH_PREFIX)
    ? sessionNavKey(pathname.slice(SESSION_PATH_PREFIX.length))
    : pathname.startsWith(PROJECT_PATH_PREFIX)
      ? projectNavKey(pathname.slice(PROJECT_PATH_PREFIX.length))
      : (NAV_KEY_BY_PATH[pathname] ?? null)

  // The session the debug viewer would read, when one is open. Everything else
  // is a settings page with no raw log behind it.
  const activeSessionId = activeNavKey?.startsWith("session:")
    ? activeNavKey.slice("session:".length)
    : null

  const [sessions, setSessions] = React.useState<Session[]>([])
  const [sessionsLoading, setSessionsLoading] = React.useState(true)
  const [projects, setProjects] = React.useState<Project[]>([])
  const [projectsLoading, setProjectsLoading] = React.useState(true)
  const [models, setModels] = React.useState<ModelConfig[]>([])
  const [modelsLoaded, setModelsLoaded] = React.useState(false)
  const [config, setConfig] = React.useState<FaberConfig | null>(null)
  const [me, setMe] = React.useState<Me | null>(null)
  const [creatingSession, setCreatingSession] = React.useState(false)
  const [createError, setCreateError] = React.useState<string | null>(null)

  // Only the pick is stored; the model itself is derived, so a selection made
  // before the list loaded — or one whose model was since deleted — falls back
  // to the first model rather than leaving messages with nowhere to go.
  const [pickedModel, setPickedModel] = React.useState<string | null>(null)
  const selectedModel =
    models.find((model) => model.alias === pickedModel) ?? models[0] ?? null

  // No fallback of its own: `null` means "whatever the model defaults to",
  // which is a real answer rather than a missing one.
  const [selectedThinking, setSelectedThinking] = React.useState<ThinkingSelection | null>(null)

  // Registered by the thread on screen. The command drawer targets this when it
  // exists — a pick made in a thread belongs to that thread — and the draft
  // above otherwise.
  const [threadSelection, setThreadSelection] = React.useState<SessionSelection | null>(null)

  // The debug viewer's open state. Set by the command palette; rendered by the
  // session page, which owns the viewport it overlays and the thread it reads.
  const [debugViewer, setDebugViewer] = React.useState<DebugView | null>(null)

  // The viewer belongs to the conversation it was opened over. Navigating away
  // closes it rather than letting it reappear over whatever thread comes next.
  React.useEffect(() => {
    setDebugViewer(null)
  }, [activeNavKey])

  const activeSelection: SessionSelection = threadSelection ?? {
    model: selectedModel,
    thinking: selectedThinking,
    selectModel: setPickedModel,
    selectThinking: setSelectedThinking,
    error: null,
  }

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listSessions()
      .then((rows) => {
        if (!cancelled) setSessions(rows)
      })
      .catch(() => {
        // Empty sidebar is indistinguishable from a failed fetch, which is
        // fine for a first pass — a retry happens on the next visit.
      })
      .finally(() => {
        if (!cancelled) setSessionsLoading(false)
      })

    void faber
      .listProjects()
      .then((rows) => {
        if (!cancelled) setProjects(rows)
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setProjectsLoading(false)
      })

    void faber
      .listModels()
      .then((rows) => {
        if (!cancelled) setModels(rows)
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setModelsLoaded(true)
      })

    void faber
      .config()
      .then((value) => {
        if (!cancelled) setConfig(value)
      })

    void faber
      .me()
      .then((value) => {
        if (!cancelled) setMe(value)
      })
      .catch(() => {
        // Signed out, or the API is unreachable. Either way the nav stays as
        // it is for a signed-out visitor rather than guessing at an answer.
      })

    return () => {
      cancelled = true
    }
  }, [])

  const createSession = React.useCallback(
    async (projectId?: Uuid | null): Promise<CreatedSession | null> => {
      setCreatingSession(true)
      setCreateError(null)
      try {
        const created = projectId
          ? await faber.createProjectSession(projectId)
          : await faber.createSession()
        setSessions((prev) => [created, ...prev])
        return created
      } catch (err) {
        setCreateError(err instanceof FaberError ? err.message : "failed to create a thread")
        return null
      } finally {
        setCreatingSession(false)
      }
    },
    [],
  )

  // Writes can answer out of order, so an older `rev` never replaces a newer one.
  const storeProject = React.useCallback((project: Project) => {
    setProjects((prev) => {
      const held = prev.find((row) => row.id === project.id)
      if (held && held.rev > project.rev) return prev
      return [project, ...prev.filter((row) => row.id !== project.id)]
    })
  }, [])

  // Kept in a ref as well as state so a rename or delete reads the newest
  // `rev` without the callbacks changing identity on every edit.
  const projectsRef = React.useRef(projects)
  projectsRef.current = projects
  const revOf = (id: Uuid): number => {
    const found = projectsRef.current.find((row) => row.id === id)
    if (!found) throw new FaberError(404, "project not found")
    return found.rev
  }

  const createProject = React.useCallback(
    async (name: string, description?: string): Promise<Project> => {
      const created = await faber.createProject({ name: name.trim(), description })
      setProjects((prev) => [created, ...prev])
      return created
    },
    [],
  )

  const renameProject = React.useCallback(
    async (id: Uuid, name: string): Promise<Project> => {
      const updated = await faber.updateProject(id, revOf(id), { name: name.trim() })
      storeProject(updated)
      return updated
    },
    [storeProject],
  )

  const deleteProject = React.useCallback(
    async (id: Uuid): Promise<void> => {
      await faber.deleteProject(id, revOf(id))
      setProjects((prev) => prev.filter((row) => row.id !== id))
      const gone = new Set(
        sessions.filter((session) => session.project_id === id).map((session) => session.id),
      )
      setSessions((prev) => prev.filter((session) => session.project_id !== id))
      const leaving =
        activeNavKey === projectNavKey(id) ||
        [...gone].some((sessionId) => activeNavKey === sessionNavKey(sessionId))
      if (leaving) navigate({ to: "/" })
    },
    [sessions, activeNavKey, navigate],
  )

  const renameSession = React.useCallback(async (id: Uuid, title: string): Promise<Session> => {
    const updated = await faber.updateSession(id, { title: title.trim() || null })
    setSessions((prev) => prev.map((session) => (session.id === id ? updated : session)))
    return updated
  }, [])

  const updateSessionTitle = React.useCallback((id: Uuid, title: string) => {
    setSessions((prev) => prev.map((session) => (session.id === id ? { ...session, title } : session)))
  }, [])

  const deleteSession = React.useCallback(
    async (id: Uuid): Promise<void> => {
      await faber.deleteSession(id)
      setSessions((prev) => prev.filter((session) => session.id !== id))
      if (activeNavKey === sessionNavKey(id)) navigate({ to: "/" })
    },
    [activeNavKey, navigate],
  )

  const addModel = React.useCallback(async (body: CreateModelRequest): Promise<ModelConfig> => {
    const created = await faber.createModel(body)
    setModels((prev) => [...prev, created])
    return created
  }, [])

  const editModel = React.useCallback(
    async (id: Uuid, patch: UpdateModelRequest): Promise<ModelConfig> => {
      const updated = await faber.updateModel(id, patch)
      setModels((prev) => prev.map((model) => (model.id === id ? updated : model)))
      return updated
    },
    [],
  )

  const removeModel = React.useCallback(async (id: Uuid): Promise<void> => {
    await faber.deleteModel(id)
    setModels((prev) => prev.filter((model) => model.id !== id))
  }, [])

  const value = React.useMemo<AppShellContextValue>(
    () => ({
      config,
      me,
      sessions,
      sessionsLoading,
      projects,
      projectsLoading,
      models,
      modelsLoaded,
      selectedModel,
      selectModel: setPickedModel,
      selectedThinking,
      selectThinking: setSelectedThinking,
      creatingSession,
      createError,
      createSession,
      renameSession,
      updateSessionTitle,
      deleteSession,
      createProject,
      renameProject,
      deleteProject,
      storeProject,
      addModel,
      editModel,
      removeModel,
      threadSelection,
      setThreadSelection,
      debugViewer,
      setDebugViewer,
    }),
    [
      config,
      me,
      sessions,
      sessionsLoading,
      projects,
      projectsLoading,
      models,
      modelsLoaded,
      selectedModel,
      selectedThinking,
      creatingSession,
      createError,
      createSession,
      renameSession,
      updateSessionTitle,
      deleteSession,
      createProject,
      renameProject,
      deleteProject,
      storeProject,
      addModel,
      editModel,
      removeModel,
      threadSelection,
      debugViewer,
    ],
  )

  // One set of handlers for both frames: the sidebar above `md` and the top
  // bar with its command drawer below it render the same nav from the same
  // callbacks, so the two can never drift.
  const openSession = (created: CreatedSession | null) => {
    if (created) navigate({ to: "/session/$sessionId", params: { sessionId: created.id } })
  }

  const nav = {
    sessions,
    projects,
    activeNavKey,
    onSelectSession: (id: Uuid) =>
      navigate({ to: "/session/$sessionId", params: { sessionId: id } }),
    onSelectProject: (id: Uuid) =>
      navigate({ to: "/project/$projectId", params: { projectId: id } }),
    onSelectModels: () => navigate({ to: "/models" }),
    onSelectCredentials: () => navigate({ to: "/credentials" }),
    onSelectEnvironments: () => navigate({ to: "/environments" }),
    onCreateSession: () => void createSession().then(openSession),
    onCreateProjectSession: (projectId: Uuid) => void createSession(projectId).then(openSession),
    onCreateProject: async (name: string, description?: string) => {
      const created = await createProject(name, description)
      navigate({ to: "/project/$projectId", params: { projectId: created.id } })
      return created
    },
    onRenameSession: renameSession,
    onDeleteSession: deleteSession,
    onRenameProject: renameProject,
    onDeleteProject: deleteProject,
    creating: creatingSession,
  }

  return (
    <AppShellContext.Provider value={value}>
      {/* Summoned with ⌘K / Ctrl+K on desktop; targets the open thread's
          selection when there is one, the draft otherwise. */}
      <GlobalCommandDialog
        models={models}
        modelsLoaded={modelsLoaded}
        model={activeSelection.model}
        thinking={activeSelection.thinking}
        onModelSelect={activeSelection.selectModel}
        onThinkingSelect={activeSelection.selectThinking}
        onCreateSession={nav.onCreateSession}
        activeNavKey={activeNavKey}
        onSelectModels={nav.onSelectModels}
        onSelectCredentials={nav.onSelectCredentials}
        onSelectEnvironments={nav.onSelectEnvironments}
        activeSessionId={activeSessionId}
        onViewTranscripts={() => setDebugViewer("transcript")}
        onViewExchanges={() => setDebugViewer("exchange")}
      />
      <div className="relative flex min-h-0 flex-1">
        {/* Both frames stay mounted and CSS picks one, so the first paint is
            already the right shape — no effect-driven swap to flash through. */}
        <AppSidebar {...nav} loading={sessionsLoading || projectsLoading} className="hidden md:flex" />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <MobileTopBar
            {...nav}
            className="md:hidden"
            canDebug={activeSessionId !== null}
            onViewTranscripts={() => setDebugViewer("transcript")}
            onViewExchanges={() => setDebugViewer("exchange")}
          />
          {children}
        </main>
      </div>
    </AppShellContext.Provider>
  )
}
