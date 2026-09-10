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
  type Session,
  type ThinkingSelection,
  type UpdateModelRequest,
  type Uuid,
} from "@/lib/api"
import { sessionNavKey } from "@/lib/sessions/labels"
import { AppSidebar } from "@/components/shell/app-sidebar"
import { MobileTopBar } from "@/components/shell/mobile-nav"

type AppShellContextValue = {
  config: FaberConfig | null
  /** The signed-in user, or `null` until it arrives. */
  me: Me | null
  sessions: Session[]
  sessionsLoading: boolean
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
  createSession: () => Promise<CreatedSession | null>
  renameSession: (id: Uuid, title: string) => Promise<Session>
  updateSessionTitle: (id: Uuid, title: string) => void
  deleteSession: (id: Uuid) => Promise<void>
  addModel: (body: CreateModelRequest) => Promise<ModelConfig>
  editModel: (id: Uuid, patch: UpdateModelRequest) => Promise<ModelConfig>
  removeModel: (id: Uuid) => Promise<void>
}

const AppShellContext = React.createContext<AppShellContextValue | null>(null)

/** Sessions, models, and thread creation — shared by the landing page and every session page. */
export function useAppShell(): AppShellContextValue {
  const ctx = React.useContext(AppShellContext)
  if (!ctx) throw new Error("useAppShell must be used within <AppShell>")
  return ctx
}

const SESSION_PATH_PREFIX = "/session/"

/** Static routes the sidebar highlights. Sessions key off their id instead. */
const NAV_KEY_BY_PATH: Record<string, string> = {
  "/models": "models",
  "/credentials": "credentials",
  "/hosts": "hosts",
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
    : (NAV_KEY_BY_PATH[pathname] ?? null)

  const [sessions, setSessions] = React.useState<Session[]>([])
  const [sessionsLoading, setSessionsLoading] = React.useState(true)
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

  const createSession = React.useCallback(async (): Promise<CreatedSession | null> => {
    setCreatingSession(true)
    setCreateError(null)
    try {
      const created = await faber.createSession()
      setSessions((prev) => [created, ...prev])
      return created
    } catch (err) {
      setCreateError(err instanceof FaberError ? err.message : "failed to create a thread")
      return null
    } finally {
      setCreatingSession(false)
    }
  }, [])

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
      addModel,
      editModel,
      removeModel,
    }),
    [
      config,
      me,
      sessions,
      sessionsLoading,
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
      addModel,
      editModel,
      removeModel,
    ],
  )

  // One set of handlers for both frames: the sidebar above `md` and the top
  // bar with its command drawer below it render the same nav from the same
  // callbacks, so the two can never drift.
  const nav = {
    sessions,
    activeNavKey,
    onSelectSession: (id: Uuid) =>
      navigate({ to: "/session/$sessionId", params: { sessionId: id } }),
    onSelectModels: () => navigate({ to: "/models" }),
    onSelectCredentials: () => navigate({ to: "/credentials" }),
    onSelectHosts: () => navigate({ to: "/hosts" }),
    onSelectEnvironments: () => navigate({ to: "/environments" }),
    onCreateSession: () => {
      void createSession().then((created) => {
        if (created) navigate({ to: "/session/$sessionId", params: { sessionId: created.id } })
      })
    },
    onRenameSession: renameSession,
    onDeleteSession: deleteSession,
    creating: creatingSession,
  }

  return (
    <AppShellContext.Provider value={value}>
      <div className="relative flex min-h-0 flex-1">
        {/* Both frames stay mounted and CSS picks one, so the first paint is
            already the right shape — no effect-driven swap to flash through. */}
        <AppSidebar {...nav} loading={sessionsLoading} className="hidden md:flex" />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <MobileTopBar {...nav} className="md:hidden" />
          {children}
        </main>
      </div>
    </AppShellContext.Provider>
  )
}
