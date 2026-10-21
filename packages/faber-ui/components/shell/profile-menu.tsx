"use client"

import { motion } from "framer-motion"
import { ChevronsUpDown, Settings, LogOut } from "lucide-react"

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@charlotte-ui/dropdown-menu"
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar"
import { useOptionalSurgeAuth } from "@/components/ui/surge-auth"

type ProfileMenuProps = {
  displayName?: string
  email?: string
  initials?: string
  avatarUrl?: string
}

/** First letters of the first two words, e.g. "Ada Lovelace" → "AL". */
function initialsOf(name: string) {
  const letters = name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((word) => word[0])
    .join("")
  return letters.toUpperCase() || "?"
}

export function ProfileMenu({
  displayName: displayNameProp,
  email: emailProp,
  initials: initialsProp,
  avatarUrl: avatarUrlProp,
}: ProfileMenuProps) {
  // Optional so the menu still renders in isolation (stories, previews) —
  // props win over the session, and the placeholders only fill in the gap.
  const auth = useOptionalSurgeAuth()
  const identity = auth?.session?.identity

  const displayName =
    displayNameProp ?? identity?.display_name ?? identity?.username ?? "Signed in"
  const email =
    emailProp ?? (identity ? `@${identity.username}` : "Not signed in")
  const initials = initialsProp ?? initialsOf(displayName)
  const avatarUrl = avatarUrlProp ?? identity?.avatar_url ?? undefined

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <motion.button
          type="button"
          whileTap={{ scale: 0.985 }}
          className="group flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left transition-colors hover:bg-sidebar-foreground/5 focus:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label="Open profile menu"
        >
          <Avatar className="h-9 w-9 shrink-0">
            <AvatarImage src={avatarUrl} alt={displayName} />
            <AvatarFallback className="bg-accent text-accent-foreground text-sm font-semibold">
              {initials}
            </AvatarFallback>
          </Avatar>
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-medium text-sidebar-foreground">
              {displayName}
            </div>
            <div className="truncate text-xs text-muted-foreground">{email}</div>
          </div>
          <ChevronsUpDown
            className="h-4 w-4 shrink-0 text-muted-foreground transition-transform group-data-[state=open]:rotate-180"
            aria-hidden
          />
        </motion.button>
      </DropdownMenuTrigger>

      <DropdownMenuContent align="end" side="top" sideOffset={8} className="w-64">
        <DropdownMenuLabel className="flex items-center gap-3 px-2 py-2">
          <Avatar className="h-10 w-10">
            <AvatarImage src={avatarUrl} alt={displayName} />
            <AvatarFallback className="bg-accent text-accent-foreground text-sm font-semibold">
              {initials}
            </AvatarFallback>
          </Avatar>
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-medium">{displayName}</div>
            <div className="truncate text-xs text-muted-foreground">{email}</div>
          </div>
        </DropdownMenuLabel>

        <DropdownMenuSeparator />

        <DropdownMenuItem>
          <Settings className="h-4 w-4" />
          Settings
          <span className="ml-auto text-[10px] text-muted-foreground">⌘,</span>
        </DropdownMenuItem>

        <DropdownMenuSeparator />

        <DropdownMenuItem
          variant="destructive"
          disabled={!auth}
          onSelect={() => void auth?.logout()}
        >
          <LogOut className="h-4 w-4" />
          Sign out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
