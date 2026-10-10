"use client"

import * as React from "react"
import {
  ArrowLeftRight,
  Cpu,
  Folder,
  FolderOpen,
  FolderPlus,
  KeyRound,
  Layers,
  LogOut,
  Menu,
  MessageSquare,
  MessageSquareText,
  MoreHorizontal,
  Pencil,
  Plus,
  ScrollText,
  Server,
  Settings2,
  Trash2,
} from "lucide-react"

import type { Project, Session, Uuid } from "@/lib/api"
import { cn } from "@/lib/utils"
import { projectNavKey, sessionLabel, sessionNavKey } from "@/lib/sessions/labels"
import { useIsMobile } from "@/lib/use-is-mobile"
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar"
import { Button } from "@/components/ui/button"
import { FaberLogo } from "@/components/ui/logos"
import {
  CommandDrawer,
  CommandDrawerContent,
  CommandDrawerGroup,
  CommandDrawerItem,
  CommandDrawerNest,
} from "@/components/ui/command-drawer"
import { useOptionalSurgeAuth } from "@/components/ui/surge-auth"
import {
  DeleteSessionDialog,
  RenameSessionDialog,
} from "@/components/shell/session-dialogs"
import { CreateProjectDialog } from "@/components/shell/project-dialogs"

/**
 * The app frame below `md`, where a 256px sidebar would leave nothing for the
 * thread: a top bar plus the command drawer it opens.
 *
 * The drawer is the sidebar's whole surface re-cut for a thumb — the same
 * rows, reachable from the bottom of the screen rather than the left edge —
 * with two departures the shape of the input forces:
 *
 *  - Deep nav (models, credentials, hosts, environments) moves behind a
 *    "Settings" level, so the root stays threads-first and one tap from open.
 *  - Rename and delete hang off the top bar's `⋯` for the thread on screen,
 *    because their desktop home is a right-click the touch surface has no way
 *    to reach. A project's own rename and delete live on its page.
 *  - A project is a nest rather than an inline fold: its page, a new thread
 *    in it, and its threads, one level in.
 *
 * Everything inside a `CommandDrawerGroup` is a `CommandDrawerItem` or a
 * `CommandDrawerNest` by construction: those two hide themselves when the
 * level they were declared in isn't the one on screen, and any other child
 * would keep rendering after a nest is pushed.
 */

/** Static routes the drawer highlights, keyed as `AppShell` keys them. */
const NAV_ITEMS = [
  { key: "models", label: "Models", icon: Cpu },
  { key: "credentials", label: "Credentials", icon: KeyRound },
  { key: "hosts", label: "Hosts", icon: Server },
  { key: "environments", label: "Environments", icon: Layers },
] as const

/** Threads shown at the drawer's root before the rest move behind "All threads". */
const RECENT_THREADS = 5

export type MobileTopBarProps = {
  sessions: Session[]
  projects: Project[]
  /** Matches `AppSidebar`: `sessionNavKey(id)`, a static nav key, or `null`. */
  activeNavKey: string | null
  onSelectSession: (id: Uuid) => void
  onSelectProject: (id: Uuid) => void
  onSelectModels: () => void
  onSelectCredentials: () => void
  onSelectHosts: () => void
  onSelectEnvironments: () => void
  onCreateSession: () => void
  onCreateProjectSession: (projectId: Uuid) => void
  onCreateProject: (name: string, description?: string) => Promise<Project>
  onRenameSession: (id: Uuid, title: string) => Promise<Session>
  onDeleteSession: (id: Uuid) => Promise<void>
  /** Whether the debug entries are offered — true only with a thread open. */
  canDebug?: boolean
  onViewTranscripts: () => void
  onViewExchanges: () => void
  creating?: boolean
  className?: string
}

export function MobileTopBar({
  sessions,
  projects,
  activeNavKey,
  onSelectSession,
  onSelectProject,
  onSelectModels,
  onSelectCredentials,
  onSelectHosts,
  onSelectEnvironments,
  onCreateSession,
  onCreateProjectSession,
  onCreateProject,
  onRenameSession,
  onDeleteSession,
  canDebug = false,
  onViewTranscripts,
  onViewExchanges,
  creating = false,
  className,
}: MobileTopBarProps) {
  const [navOpen, setNavOpen] = React.useState(false)
  const [actionsOpen, setActionsOpen] = React.useState(false)
  const [renameTarget, setRenameTarget] = React.useState<Session | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Session | null>(null)
  const [createProjectOpen, setCreateProjectOpen] = React.useState(false)

  const isMobile = useIsMobile()

  // A drawer left open behind the sidebar on rotate or resize would be a modal
  // nobody can see to dismiss.
  React.useEffect(() => {
    if (!isMobile) {
      setNavOpen(false)
      setActionsOpen(false)
    }
  }, [isMobile])

  const activeSession =
    sessions.find((session) => sessionNavKey(session.id) === activeNavKey) ?? null
  const activeProject =
    projects.find((project) => projectNavKey(project.id) === activeNavKey) ?? null

  const onSelectNav: Record<string, () => void> = {
    models: onSelectModels,
    credentials: onSelectCredentials,
    hosts: onSelectHosts,
    environments: onSelectEnvironments,
  }

  // The bar names the thing nothing else on screen names. A settings page
  // writes its own `h1` a few pixels lower, so repeating it here would say the
  // same word twice; a thread has no heading at all, so its title goes here.
  const title = activeSession
    ? sessionLabel(activeSession)
    : activeProject
      ? activeProject.name
      : "Panit"

  /** Every drawer row navigates, so every drawer row closes the drawer first. */
  const close = (run: () => void) => () => {
    setNavOpen(false)
    run()
  }

  // Project threads are reached through their project, as in the sidebar.
  const looseSessions = sessions.filter((session) => !session.project_id)
  const recent = looseSessions.slice(0, RECENT_THREADS)

  const threadItem = (session: Session) => {
    const active = sessionNavKey(session.id) === activeNavKey
    return (
      <CommandDrawerItem
        key={session.id}
        icon={active ? <MessageSquareText /> : <MessageSquare />}
        iconClassName={active ? "text-primary" : undefined}
        label={sessionLabel(session)}
        aria-current={active ? "page" : undefined}
        className={cn(active && "bg-accent text-foreground")}
        onSelect={close(() => onSelectSession(session.id))}
      />
    )
  }

  return (
    <>
      <header
        className={cn(
          "flex h-14 shrink-0 items-center gap-1 border-b border-border bg-background/80 px-2 backdrop-blur-sm",
          className,
        )}
      >
        <Button
          variant="ghost"
          size="icon-lg"
          aria-label="Open menu"
          aria-expanded={navOpen}
          onClick={() => setNavOpen(true)}
        >
          <Menu className="size-5" />
        </Button>

        {activeSession || activeProject ? null : <FaberLogo size={22} aria-hidden className="ml-1 shrink-0" />}
        <span className="min-w-0 flex-1 truncate px-1.5 text-[15px] font-semibold tracking-tight">
          {title}
        </span>

        {activeSession ? (
          <Button
            variant="ghost"
            size="icon-lg"
            aria-label={`Actions for ${sessionLabel(activeSession)}`}
            onClick={() => setActionsOpen(true)}
          >
            <MoreHorizontal className="size-5" />
          </Button>
        ) : null}

        <Button
          variant="ghost"
          size="icon-lg"
          aria-label="New thread"
          disabled={creating}
          onClick={onCreateSession}
        >
          <Plus className="size-5" />
        </Button>
      </header>

      {/* ── The sidebar, re-cut as a bottom sheet ───────────────────────── */}
      <CommandDrawer open={navOpen} onOpenChange={setNavOpen}>
        <CommandDrawerContent
          title="Panit"
          description="Threads, settings, and your account"
        >
          <CommandDrawerGroup>
            <CommandDrawerItem
              icon={<Plus />}
              label="New thread"
              description="Start a fresh conversation"
              disabled={creating}
              onSelect={close(onCreateSession)}
            />
          </CommandDrawerGroup>

          <CommandDrawerGroup>
            {projects.map((project) => {
              const projectThreads = sessions.filter(
                (session) => session.project_id === project.id,
              )
              const active =
                activeNavKey === projectNavKey(project.id) ||
                projectThreads.some((session) => sessionNavKey(session.id) === activeNavKey)
              return (
                <CommandDrawerNest
                  key={project.id}
                  label={project.name}
                  icon={active ? <FolderOpen /> : <Folder />}
                  iconClassName={active ? "text-primary" : undefined}
                  description={
                    projectThreads.length === 1 ? "1 thread" : `${projectThreads.length} threads`
                  }
                >
                  <CommandDrawerGroup>
                    <CommandDrawerItem
                      icon={<Plus />}
                      label="New thread"
                      description={`Start one in ${project.name}`}
                      disabled={creating}
                      onSelect={close(() => onCreateProjectSession(project.id))}
                    />
                    <CommandDrawerItem
                      icon={<Settings2 />}
                      label="Project settings"
                      description="Plugins, name and description"
                      onSelect={close(() => onSelectProject(project.id))}
                    />
                  </CommandDrawerGroup>
                  {projectThreads.length > 0 ? (
                    <CommandDrawerGroup>{projectThreads.map(threadItem)}</CommandDrawerGroup>
                  ) : null}
                </CommandDrawerNest>
              )
            })}
            <CommandDrawerItem
              icon={<FolderPlus />}
              label="New project"
              onSelect={() => {
                setNavOpen(false)
                setCreateProjectOpen(true)
              }}
            />
          </CommandDrawerGroup>

          {looseSessions.length > 0 ? (
            <CommandDrawerGroup>
              {recent.map(threadItem)}
              {looseSessions.length > RECENT_THREADS ? (
                <CommandDrawerNest
                  label="All threads"
                  icon={<MessageSquare />}
                  description={`${looseSessions.length} threads`}
                >
                  <CommandDrawerGroup>{looseSessions.map(threadItem)}</CommandDrawerGroup>
                </CommandDrawerNest>
              ) : null}
            </CommandDrawerGroup>
          ) : null}

          <CommandDrawerGroup>
            <CommandDrawerNest
              label="Settings"
              icon={<Settings2 />}
              description="Models, credentials, hosts, environments"
            >
              <CommandDrawerGroup>
                {NAV_ITEMS.map(({ key, label, icon: Icon }) => {
                  const active = activeNavKey === key
                  return (
                    <CommandDrawerItem
                      key={key}
                      icon={<Icon />}
                      iconClassName={active ? "text-primary" : undefined}
                      label={label}
                      aria-current={active ? "page" : undefined}
                      className={cn(active && "bg-accent text-foreground")}
                      onSelect={close(onSelectNav[key])}
                    />
                  )
                })}
              </CommandDrawerGroup>
            </CommandDrawerNest>

            <AccountNest />
          </CommandDrawerGroup>
        </CommandDrawerContent>
      </CommandDrawer>

      {/* ── Thread actions: the desktop right-click, as a sheet ──────────── */}
      <CommandDrawer open={actionsOpen} onOpenChange={setActionsOpen}>
        <CommandDrawerContent
          title={activeSession ? sessionLabel(activeSession) : "Thread"}
          description="Thread actions"
        >
          <CommandDrawerGroup>
            <CommandDrawerItem
              icon={<Pencil />}
              label="Rename"
              onSelect={() => {
                setActionsOpen(false)
                setRenameTarget(activeSession)
              }}
            />
            <CommandDrawerItem
              icon={<Trash2 />}
              label="Delete"
              destructive
              onSelect={() => {
                setActionsOpen(false)
                setDeleteTarget(activeSession)
              }}
            />
          </CommandDrawerGroup>

          {canDebug ? (
            <CommandDrawerGroup>
              <CommandDrawerItem
                icon={<ScrollText />}
                label="View transcripts"
                description="What the user saw, raw"
                onSelect={() => {
                  setActionsOpen(false)
                  onViewTranscripts()
                }}
              />
              <CommandDrawerItem
                icon={<ArrowLeftRight />}
                label="View exchanges"
                description="Requests and provider events, raw"
                onSelect={() => {
                  setActionsOpen(false)
                  onViewExchanges()
                }}
              />
            </CommandDrawerGroup>
          ) : null}
        </CommandDrawerContent>
      </CommandDrawer>

      <RenameSessionDialog
        key={renameTarget?.id ?? "none"}
        session={renameTarget}
        onOpenChange={(open) => !open && setRenameTarget(null)}
        onRename={onRenameSession}
      />

      <DeleteSessionDialog
        session={deleteTarget}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
        onDelete={onDeleteSession}
      />

      <CreateProjectDialog
        key={createProjectOpen ? "open" : "closed"}
        open={createProjectOpen}
        onOpenChange={setCreateProjectOpen}
        onCreate={onCreateProject}
      />
    </>
  )
}

/**
 * The profile menu's drawer half: identity on the trigger row, sign-out one
 * level in, so a destructive action is never the thing under a stray thumb.
 */
function AccountNest() {
  const auth = useOptionalSurgeAuth()
  const identity = auth?.session?.identity

  const displayName = identity?.display_name ?? identity?.username ?? "Signed in"
  const handle = identity ? `@${identity.username}` : "Not signed in"
  const initials =
    displayName
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((word) => word[0])
      .join("")
      .toUpperCase() || "?"

  return (
    <CommandDrawerNest
      label={displayName}
      description={handle}
      iconClassName="bg-transparent"
      icon={
        <Avatar className="size-8 rounded-lg">
          <AvatarImage src={identity?.avatar_url ?? undefined} alt={displayName} />
          <AvatarFallback className="rounded-lg bg-accent text-xs font-semibold text-accent-foreground">
            {initials}
          </AvatarFallback>
        </Avatar>
      }
    >
      <CommandDrawerGroup>
        <CommandDrawerItem
          icon={<LogOut />}
          label="Sign out"
          destructive
          disabled={!auth}
          onSelect={() => void auth?.logout()}
        />
      </CommandDrawerGroup>
    </CommandDrawerNest>
  )
}
