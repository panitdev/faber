"use client"

import * as React from "react"
import {
  Cpu,
  KeyRound,
  Layers,
  MessageSquare,
  MessageSquareText,
  MoreHorizontal,
  Pencil,
  Plus,
  Server,
  Trash2,
} from "lucide-react"
import { motion } from "framer-motion"

import type { Session, Uuid } from "@/lib/api"
import { cn } from "@/lib/utils"
import { sessionLabel, sessionNavKey } from "@/lib/sessions/labels"
import { FaberLogo } from "@/components/ui/logos"
import { Button } from "@/components/ui/button"
import { SidebarNav } from "@/components/ui/sidebar-nav"
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu"
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

  return (
    <aside
      className={cn(
        "flex h-full w-64 shrink-0 flex-col overflow-hidden border-r border-border bg-sidebar/60 text-sidebar-foreground",
        className,
      )}
    >
      <div className="flex items-center gap-3 px-5 pt-5 pb-4">
        <FaberLogo size={28} aria-hidden />
        <span className="text-[15px] font-semibold tracking-tight">Faber</span>
      </div>

      <div className="px-3 pb-3">
        <Button
          size="lg"
          className="w-full justify-start gap-2"
          onClick={onCreateSession}
          loading={creating}
          loadingText="New thread"
        >
          <Plus className="h-4 w-4" />
          New thread
        </Button>
      </div>

      <div className="px-2 pb-2">
        <SidebarNav
          ariaLabel="Primary navigation"
          className="rounded-none border-none bg-transparent p-0"
          sections={[
            {
              items: [
                {
                  label: "Models",
                  icon: Cpu,
                  active: activeNavKey === "models",
                  onClick: onSelectModels,
                },
                {
                  label: "Credentials",
                  icon: KeyRound,
                  active: activeNavKey === "credentials",
                  onClick: onSelectCredentials,
                },
                {
                  label: "Hosts",
                  icon: Server,
                  active: activeNavKey === "hosts",
                  onClick: onSelectHosts,
                },
                {
                  label: "Environments",
                  icon: Layers,
                  active: activeNavKey === "environments",
                  onClick: onSelectEnvironments,
                },
              ],
            },
          ]}
        />
      </div>

      {!loading && sessions.length === 0 ? (
        <div className="min-h-0 flex-1 overflow-y-auto px-5 pt-2">
          <p className="text-[13px] text-muted-foreground">No threads yet.</p>
        </div>
      ) : (
        <nav
          aria-label="Sidebar navigation"
          className="min-h-0 flex-1 overflow-y-auto rounded-none border-none bg-transparent p-0 px-2 py-1"
        >
          {!loading ? (
            <div>
              <div className="px-2 py-2 text-[10.5px] font-medium uppercase tracking-[0.12em] text-muted-foreground/70">
                Threads
              </div>
              <div className="space-y-0.5">
                {sessions.map((session) => (
                  <ThreadRow
                    key={session.id}
                    session={session}
                    active={sessionNavKey(session.id) === activeNavKey}
                    onSelect={() => onSelectSession(session.id)}
                    onRename={() => setRenameTarget(session)}
                    onDelete={() => setDeleteTarget(session)}
                  />
                ))}
              </div>
            </div>
          ) : null}
        </nav>
      )}

      <div className="border-t border-sidebar-border p-2">
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

function ThreadRow({
  session,
  active,
  onSelect,
  onRename,
  onDelete,
}: {
  session: Session
  active: boolean
  onSelect: () => void
  onRename: () => void
  onDelete: () => void
}) {
  const Icon = active ? MessageSquareText : MessageSquare

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          className={cn(
            "group relative flex items-center rounded-lg transition-colors",
            active
              ? "bg-sidebar-accent font-medium text-foreground"
              : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-foreground"
          )}
        >
          {active ? (
            <motion.span
              layoutId="sidebar-active"
              className="absolute inset-y-1 left-0 w-0.5 rounded-full bg-primary"
              transition={{ type: "spring", stiffness: 380, damping: 30 }}
            />
          ) : null}
          <button
            type="button"
            onClick={onSelect}
            aria-current={active ? "page" : undefined}
            className="flex min-w-0 flex-1 items-center gap-2.5 px-3 py-2.5 text-left text-[13.5px]"
          >
            <Icon className={cn("h-4 w-4 shrink-0", active && "text-primary")} />
            <span className="min-w-0 flex-1 truncate">{sessionLabel(session)}</span>
          </button>

          <button
            type="button"
            aria-label={`More actions for ${sessionLabel(session)}`}
            className="mr-1.5 shrink-0 rounded-md p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-sidebar-foreground/10 hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none group-hover:opacity-100"
            onClick={(event) => {
              event.stopPropagation()
              // Opens the same context menu at the button's position, so the
              // trigger button and a right-click on the row share one menu.
              const { left, bottom } = event.currentTarget.getBoundingClientRect()
              event.currentTarget.dispatchEvent(
                new MouseEvent("contextmenu", {
                  bubbles: true,
                  clientX: left,
                  clientY: bottom,
                })
              )
            }}
          >
            <MoreHorizontal className="h-4 w-4" />
          </button>
        </div>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem onSelect={onRename}>
          <Pencil className="h-4 w-4" />
          Rename
        </ContextMenuItem>
        <ContextMenuItem variant="destructive" onSelect={onDelete}>
          <Trash2 className="h-4 w-4" />
          Delete
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  )
}
