"use client"

import * as React from "react"
import {
  ArrowLeftRight,
  Check,
  Cpu,
  KeyRound,
  Layers,
  Lightbulb,
  Plus,
  ScrollText,
  Server,
} from "lucide-react"

import type { ModelConfig, ThinkingSelection, Uuid } from "@/lib/api"
import { selectionLabel, selectionsFor, thinkingOf } from "@/lib/models/thinking"
import { useIsMobile } from "@/lib/use-is-mobile"
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandNest,
} from "@/components/ui/command"

/** Static routes the palette reaches, keyed as `AppShell` keys them. */
const NAV_ITEMS = [
  { key: "models", label: "Go to Models", icon: Cpu },
  { key: "credentials", label: "Go to Credentials", icon: KeyRound },
  { key: "hosts", label: "Go to Hosts", icon: Server },
  { key: "environments", label: "Go to Environments", icon: Layers },
] as const

export type GlobalCommandDialogProps = {
  models: ModelConfig[]
  modelsLoaded: boolean
  /** The model in effect — the open thread's, else the shell's draft. */
  model: ModelConfig | null
  /** The thinking knob in effect; `null` is the model's own default. */
  thinking: ThinkingSelection | null
  onModelSelect: (alias: string) => void
  onThinkingSelect: (selection: ThinkingSelection | null) => void
  onCreateSession: () => void
  /** Matches `AppShell`: a static nav key, a session key, or `null`. */
  activeNavKey: string | null
  onSelectModels: () => void
  onSelectCredentials: () => void
  onSelectHosts: () => void
  onSelectEnvironments: () => void
  /** The session whose raw logs the debug entries would open, if any. */
  activeSessionId: Uuid | null
  onViewTranscripts: () => void
  onViewExchanges: () => void
}

/**
 * The desktop command palette, summoned by ⌘K / Ctrl+K.
 *
 * Its rails are the two knobs that decide how the next message runs — model and
 * reasoning — plus the actions and routes that are otherwise a step away. The
 * model and reasoning rows write to whichever selection is in effect: the open
 * thread's own, or the shell's draft on the landing page. `AppShell` hands that
 * down, so the palette never has to know which page it is over.
 *
 * Desktop only: below `md` the top bar's command drawer already owns this
 * surface, and a modal fighting a bottom sheet would be one frame too many.
 */
export function GlobalCommandDialog({
  models,
  modelsLoaded,
  model,
  thinking,
  onModelSelect,
  onThinkingSelect,
  onCreateSession,
  activeNavKey,
  onSelectModels,
  onSelectCredentials,
  onSelectHosts,
  onSelectEnvironments,
  activeSessionId,
  onViewTranscripts,
  onViewExchanges,
}: GlobalCommandDialogProps) {
  const [open, setOpen] = React.useState(false)
  const isMobile = useIsMobile()

  // Global hotkey. Fires even while a field has focus — the palette is meant to
  // reach the user wherever they are — and swallows the browser's own binding.
  React.useEffect(() => {
    if (isMobile) return

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== "k") return
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return
      event.preventDefault()
      setOpen((current) => !current)
    }

    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [isMobile])

  // A palette left open behind the mobile frame would be a modal nobody can see
  // to dismiss.
  React.useEffect(() => {
    if (isMobile) setOpen(false)
  }, [isMobile])

  if (isMobile) return null

  /** Every terminal row closes the palette before it acts. */
  const close = (run: () => void) => () => {
    setOpen(false)
    run()
  }

  const capability = thinkingOf(model)
  const thinkingOptions = selectionsFor(capability)

  const onSelectNav: Record<string, () => void> = {
    models: onSelectModels,
    credentials: onSelectCredentials,
    hosts: onSelectHosts,
    environments: onSelectEnvironments,
  }

  return (
    <CommandDialog
      open={open}
      onOpenChange={setOpen}
      title="Commands"
      description="Model, reasoning, and navigation"
    >
      <Command className="rounded-none border-0 bg-transparent shadow-none">
        <CommandInput placeholder="Type a command or search…" />
        <CommandList>
          <CommandEmpty>No results found.</CommandEmpty>

          <CommandGroup heading="Actions">
            <CommandNest
              label="Set model"
              icon={<Cpu />}
              placeholder="Search models…"
              keywords={["model", "alias"]}
            >
              <CommandGroup>
                {models.map((candidate) => {
                  const active = candidate.id === model?.id
                  return (
                    <CommandItem
                      key={candidate.id}
                      value={`${candidate.alias} ${candidate.wire_id}`}
                      onSelect={close(() => onModelSelect(candidate.alias))}
                    >
                      {active ? <Check className="!text-primary" /> : <Cpu />}
                      <span className="min-w-0 flex-1 truncate">{candidate.alias}</span>
                      <span className="truncate text-xs text-muted-foreground">
                        {candidate.wire_id}
                      </span>
                    </CommandItem>
                  )
                })}
              </CommandGroup>
            </CommandNest>

            {thinkingOptions.length > 0 ? (
              <CommandNest
                label="Set model reasoning"
                icon={<Lightbulb />}
                placeholder="Search reasoning levels…"
                keywords={["reasoning", "thinking", "effort"]}
              >
                <CommandGroup>
                  <CommandItem value="Default" onSelect={close(() => onThinkingSelect(null))}>
                    {thinking === null ? <Check className="!text-primary" /> : <Lightbulb />}
                    <span className="min-w-0 flex-1 truncate">{selectionLabel(null)}</span>
                  </CommandItem>
                  {thinkingOptions.map((option) => {
                    const active = option === thinking
                    return (
                      <CommandItem
                        key={option}
                        value={selectionLabel(option)}
                        onSelect={close(() => onThinkingSelect(option))}
                      >
                        {active ? <Check className="!text-primary" /> : <Lightbulb />}
                        <span className="min-w-0 flex-1 truncate">{selectionLabel(option)}</span>
                      </CommandItem>
                    )
                  })}
                </CommandGroup>
              </CommandNest>
            ) : (
              <CommandItem value="Set model reasoning" disabled>
                <Lightbulb />
                <span className="min-w-0 flex-1 truncate">Set model reasoning</span>
                <span className="truncate text-xs text-muted-foreground">
                  {model
                    ? "This model has no reasoning levels"
                    : modelsLoaded
                      ? "Select a model first"
                      : "Loading models…"}
                </span>
              </CommandItem>
            )}

            <CommandItem value="New thread" onSelect={close(onCreateSession)}>
              <Plus />
              <span className="min-w-0 flex-1 truncate">New thread</span>
            </CommandItem>
          </CommandGroup>

          <CommandGroup heading="Debug">
            <CommandItem
              value="View transcripts"
              disabled={!activeSessionId}
              onSelect={close(onViewTranscripts)}
            >
              <ScrollText />
              <span className="min-w-0 flex-1 truncate">View transcripts</span>
              {!activeSessionId ? (
                <span className="truncate text-xs text-muted-foreground">
                  Open a thread first
                </span>
              ) : null}
            </CommandItem>
            <CommandItem
              value="View exchanges"
              disabled={!activeSessionId}
              onSelect={close(onViewExchanges)}
            >
              <ArrowLeftRight />
              <span className="min-w-0 flex-1 truncate">View exchanges</span>
              {!activeSessionId ? (
                <span className="truncate text-xs text-muted-foreground">
                  Open a thread first
                </span>
              ) : null}
            </CommandItem>
          </CommandGroup>

          <CommandGroup heading="Navigation">
            {NAV_ITEMS.map(({ key, label, icon: Icon }) => {
              const active = activeNavKey === key
              return (
                <CommandItem
                  key={key}
                  value={label}
                  aria-current={active ? "page" : undefined}
                  onSelect={close(onSelectNav[key])}
                >
                  {active ? <Check className="!text-primary" /> : <Icon />}
                  <span className="min-w-0 flex-1 truncate">{label}</span>
                </CommandItem>
              )
            })}
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  )
}
