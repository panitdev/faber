"use client"

import * as React from "react"
import {
  Cpu,
  KeyRound,
  Layers,
  LogOut,
  Menu,
  MessageSquare,
  MessageSquareText,
  MoreHorizontal,
  Pencil,
  Plus,
  Server,
  Settings2,
  Trash2,
} from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { cn } from "@/lib/utils"
import { sessionLabel, sessionNavKey } from "@/lib/sessions/labels"
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
 *    to reach.
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
  /** Matches `AppSidebar`: `sessionNavKey(id)`, a static nav key, or `null`. */
  activeNavKey: string | null
  onSelectSession: (id: Uuid) => void
  onSelectModels: () => void
  onSelectCredentials: () => void
  onSelectHosts: () => void
  onSelectEnvironments: () => void
  onCreateSession: () => void
  onRenameSession: (id: Uuid, title: string) => Promise<Session>
  onDeleteSession: (id: Uuid) => Promise<void>
  creating?: boolean
  className?: string
}

export function MobileTopBar({
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
  creating = false,
  className,
}: MobileTopBarProps) {
  const [navOpen, setNavOpen] = React.useState(false)
  const [actionsOpen, setActionsOpen] = React.useState(false)
  const [renameTarget, setRenameTarget] = React.useState<Session | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Session | null>(null)

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

  const onSelectNav: Record<string, () => void> = {
    models: onSelectModels,
    credentials: onSelectCredentials,
    hosts: onSelectHosts,
    environments: onSelectEnvironments,
  }

  // The bar names the thing nothing else on screen names. A settings page
  // writes its own `h1` a few pixels lower, so repeating it here would say the
  // same word twice; a thread has no heading at all, so its title goes here.
  const title = activeSession ? sessionLabel(activeSession) : "Faber"

  /** Every drawer row navigates, so every drawer row closes the drawer first. */
  const close = (run: () => void) => () => {
    setNavOpen(false)
    run()
  }

  const recent = sessions.slice(0, RECENT_THREADS)

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

        {activeSession ? null : <FaberLogo size={22} aria-hidden className="ml-1 shrink-0" />}
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
          title="Faber"
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

          {sessions.length > 0 ? (
            <CommandDrawerGroup>
              {recent.map(threadItem)}
              {sessions.length > RECENT_THREADS ? (
                <CommandDrawerNest
                  label="All threads"
                  icon={<MessageSquare />}
                  description={`${sessions.length} threads`}
                >
                  <CommandDrawerGroup>{sessions.map(threadItem)}</CommandDrawerGroup>
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
