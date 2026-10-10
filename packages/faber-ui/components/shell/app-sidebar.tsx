"use client"

import * as React from "react"
import {
  Cpu,
  Folder,
  KeyRound,
  Layers,
  MoreHorizontal,
  Pencil,
  Plus,
  Server,
  Trash2,
} from "lucide-react"

import type { Project, Session, Uuid } from "@/lib/api"
import { cn } from "@/lib/utils"
import { projectNavKey, sessionLabel, sessionNavKey } from "@/lib/sessions/labels"
import { FaberLogo } from "@/components/ui/logos"
import { Button } from "@/components/ui/button"
import { SidebarNav } from "@/components/ui/sidebar-nav"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { ProfileMenu } from "@/components/shell/profile-menu"
import {
  DeleteSessionDialog,
  RenameSessionDialog,
} from "@/components/shell/session-dialogs"
import {
  CreateProjectDialog,
  DeleteProjectDialog,
  RenameProjectDialog,
} from "@/components/shell/project-dialogs"

/** Threads listed under a project before the rest move behind "View all". */
const PROJECT_THREADS = 10

export type AppSidebarProps = {
  sessions: Session[]
  projects: Project[]
  /**
   * Which row is current — `sessionNavKey(id)`, `projectNavKey(id)`, one of
   * the static nav keys (`"models"`, `"credentials"`, `"hosts"`,
   * `"environments"`), or `null`.
   */
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
  onRenameProject: (id: Uuid, name: string) => Promise<Project>
  onDeleteProject: (id: Uuid) => Promise<void>
  loading?: boolean
  creating?: boolean
  /** Lets the frame hide the sidebar where it doesn't fit — see `AppShell`. */
  className?: string
}

/*
 * The sidebar follows SidebarNav's grid: surfaces at 8px from the edge
 * (the aside's `px-2`), content at 16px. The header and button sit on the
 * same grid so every left edge lines up.
 */
export function AppSidebar({
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
  onRenameProject,
  onDeleteProject,
  loading = false,
  creating = false,
  className,
}: AppSidebarProps) {
  const [renameTarget, setRenameTarget] = React.useState<Session | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Session | null>(null)

  const [createProjectOpen, setCreateProjectOpen] = React.useState(false)
  const [renameProjectTarget, setRenameProjectTarget] = React.useState<Project | null>(null)
  const [deleteProjectTarget, setDeleteProjectTarget] = React.useState<Project | null>(null)

  const threadItem = (session: Session) => {
    const label = sessionLabel(session)
    return {
      label,
      active: sessionNavKey(session.id) === activeNavKey,
      onClick: () => onSelectSession(session.id),
      actions: (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button type="button" aria-label={`More actions for ${label}`}>
              <MoreHorizontal className="size-4" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="right" sideOffset={6}>
            <DropdownMenuItem onSelect={() => setRenameTarget(session)}>
              <Pencil className="h-4 w-4" />
              Rename
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onSelect={() => setDeleteTarget(session)}>
              <Trash2 className="h-4 w-4" />
              Delete
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ),
    }
  }

  const sessionsByProject = new Map<Uuid, Session[]>()
  for (const session of sessions) {
    if (!session.project_id) continue
    const list = sessionsByProject.get(session.project_id) ?? []
    list.push(session)
    sessionsByProject.set(session.project_id, list)
  }

  // A project row expands to its threads. Which ones are open is held here,
  // and the project holding the current thread opens itself, so following a
  // link into a project never leaves the selection hidden under a fold.
  const [openProjects, setOpenProjects] = React.useState<ReadonlySet<Uuid>>(() => new Set())
  const activeProjectId =
    projects.find(
      (project) =>
        activeNavKey === projectNavKey(project.id) ||
        sessionsByProject
          .get(project.id)
          ?.some((session) => sessionNavKey(session.id) === activeNavKey),
    )?.id ?? null
  React.useEffect(() => {
    if (!activeProjectId) return
    setOpenProjects((prev) => (prev.has(activeProjectId) ? prev : new Set(prev).add(activeProjectId)))
  }, [activeProjectId])

  const setProjectOpen = (id: Uuid, open: boolean) =>
    setOpenProjects((prev) => {
      const next = new Set(prev)
      if (open) next.add(id)
      else next.delete(id)
      return next
    })

  const projectItems = projects.map((project) => {
    const projectThreads = sessionsByProject.get(project.id) ?? []
    return {
      label: project.name,
      icon: Folder,
      active: activeNavKey === projectNavKey(project.id),
      onClick: () => onSelectProject(project.id),
      open: openProjects.has(project.id),
      onOpenChange: (open: boolean) => setProjectOpen(project.id, open),
      // The newest few, then the project page for the rest. An empty project
      // has no fold at all: its row is the way in.
      children:
        projectThreads.length === 0
          ? undefined
          : [
              ...projectThreads.slice(0, PROJECT_THREADS).map(threadItem),
              ...(projectThreads.length > PROJECT_THREADS
                ? [
                    {
                      label: "View all",
                      tone: "muted" as const,
                      onClick: () => onSelectProject(project.id),
                    },
                  ]
                : []),
            ],
      actions: (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button type="button" aria-label={`More actions for ${project.name}`}>
              <MoreHorizontal className="size-4" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="right" sideOffset={6}>
            <DropdownMenuItem onSelect={() => onCreateProjectSession(project.id)}>
              <Plus className="h-4 w-4" />
              New thread
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => setRenameProjectTarget(project)}>
              <Pencil className="h-4 w-4" />
              Rename
            </DropdownMenuItem>
            <DropdownMenuItem
              variant="destructive"
              onSelect={() => setDeleteProjectTarget(project)}
            >
              <Trash2 className="h-4 w-4" />
              Delete
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ),
    }
  })

  // Threads outside every project. A project's threads are listed under it
  // and nowhere else, so each thread has exactly one row.
  const threads = sessions.filter((session) => !session.project_id).map(threadItem)

  return (
    <aside
      className={cn(
        "flex w-60 shrink-0 flex-col self-stretch overflow-hidden border-r border-sidebar-border bg-sidebar text-sidebar-foreground",
        className,
      )}
    >
      <div className="flex h-12 shrink-0 items-center gap-2.5 px-4">
        <FaberLogo size={20} aria-hidden />
        <span className="text-[14px] font-semibold tracking-[-0.01em]">Panit</span>
      </div>

      <div className="shrink-0 px-2 pb-1">
        <Button
          size="sm"
          fullWidth
          onClick={onCreateSession}
          loading={creating}
          loadingText="New thread"
        >
          <Plus className="h-4 w-4" />
          New thread
        </Button>
      </div>

      <SidebarNav
        ariaLabel="Primary navigation"
        className="min-h-0 flex-1 bg-transparent"
        sections={[
          {
            items: [
              { label: "Models", icon: Cpu, active: activeNavKey === "models", onClick: onSelectModels },
              {
                label: "Credentials",
                icon: KeyRound,
                active: activeNavKey === "credentials",
                onClick: onSelectCredentials,
              },
              { label: "Hosts", icon: Server, active: activeNavKey === "hosts", onClick: onSelectHosts },
              {
                label: "Environments",
                icon: Layers,
                active: activeNavKey === "environments",
                onClick: onSelectEnvironments,
              },
            ],
          },
          ...(loading
            ? []
            : [
                {
                  label: "Projects",
                  items: [
                    ...projectItems,
                    {
                      label: "New project",
                      icon: Plus,
                      tone: "muted" as const,
                      onClick: () => setCreateProjectOpen(true),
                    },
                  ],
                },
                {
                  label: "Threads",
                  items:
                    threads.length > 0
                      ? threads
                      : [{ label: "No threads yet", tone: "muted" as const }],
                },
              ]),
        ]}
      />

      <div className="shrink-0 border-t border-sidebar-border p-2">
        <ProfileMenu />
      </div>

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

      <RenameProjectDialog
        key={renameProjectTarget?.id ?? "none"}
        project={renameProjectTarget}
        onOpenChange={(open) => !open && setRenameProjectTarget(null)}
        onRename={onRenameProject}
      />

      <DeleteProjectDialog
        project={deleteProjectTarget}
        threadCount={
          deleteProjectTarget ? (sessionsByProject.get(deleteProjectTarget.id)?.length ?? 0) : 0
        }
        onOpenChange={(open) => !open && setDeleteProjectTarget(null)}
        onDelete={onDeleteProject}
      />
    </aside>
  )
}
