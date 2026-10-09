"use client"

import * as React from "react"
import { Cpu, KeyRound, Layers, MoreHorizontal, Pencil, Plus, Server, Trash2 } from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { cn } from "@/lib/utils"
import { sessionLabel, sessionNavKey } from "@/lib/sessions/labels"
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

export type AppSidebarProps = {
  sessions: Session[]
  /**
   * Which row is current — `sessionNavKey(id)`, one of the static nav keys
   * (`"models"`, `"credentials"`, `"hosts"`, `"environments"`), or `null`.
   */
  activeNavKey: string | null
  onSelectSession: (id: Uuid) => void
  onSelectModels: () => void
  onSelectCredentials: () => void
  onSelectHosts: () => void
  onSelectEnvironments: () => void
  onCreateSession: () => void
  onRenameSession: (id: Uuid, title: string) => Promise<Session>
  onDeleteSession: (id: Uuid) => Promise<void>
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
  activeNavKey,
  onSelectSession,
  onSelectModels,
  onSelectCredentials,
  onSelectHosts,
  onSelectEnvironments,
  onCreateSession,
  onRenameSession,
  onDeleteSession,
  loading = false,
  creating = false,
  className,
}: AppSidebarProps) {
  const [renameTarget, setRenameTarget] = React.useState<Session | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Session | null>(null)

  const threads = sessions.map((session) => {
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
  })

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
    </aside>
  )
}
