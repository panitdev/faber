import * as React from "react"
import { AnimatePresence, motion, useReducedMotion } from "framer-motion"
import { Link } from "@tanstack/react-router"
import { ArrowLeft, Bot, FoldVertical, PanelRight, X } from "lucide-react"
import { Button } from "@charlotte-ui/button"
import { DEFAULT_EASE } from "@charlotte-ui/utils"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@charlotte-ui/labs/collapsible"

import {
  faber,
  FaberError,
  type EnvironmentCandidate,
  type RetryMode,
  type Session,
  type Uuid,
} from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import { PromptBox } from "@/components/thread/prompt-box"
import { useSessionSelection } from "@/lib/sessions/use-session-selection"
import { TurnView } from "@/components/thread/turn"
import { SessionAgentsContext } from "@/components/thread/agents"
import { AgentsPanel } from "@/components/thread/agents-panel"
import { useSessionAgents } from "@/lib/sessions/use-session-agents"
import type { MentionOption } from "@/components/thread/mention-textarea"
import { useSessionTranscript } from "@/lib/thread/use-session-transcript"
import { useStickToBottom } from "@/lib/thread/use-stick-to-bottom"
import { useCenteredTail } from "@/lib/thread/use-centered-tail"
import { summarizeUsage } from "@/lib/thread/usage"
import { UsageCard } from "@/components/thread/usage-card"
import { useIsMobile } from "@/lib/use-is-mobile"
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@charlotte-ui/dialog"
import { DebugViewerDialog } from "@/components/thread/debug-viewer"
import { CompactDialog } from "@/components/thread/compact-dialog"

/** Typed as a message, compacts instead of sending: `/compact keep the error text`. */
const COMPACT_COMMAND = /^\/compact(?:\s+([\s\S]*))?$/

export default function SessionClient({ sessionId }: { sessionId: string }) {
  const id = sessionId as Uuid

  // Keyed on the session id so navigating between threads remounts this
  // subtree — a fresh set of `useState` defaults rather than an effect that
  // resets state mid-life.
  return <SessionThread key={id} sessionId={id} />
}

function SessionThread({ sessionId }: { sessionId: Uuid }) {
  const [summaryOpen, setSummaryOpen] = React.useState(false)
  const [usageOpen, setUsageOpen] = React.useState(true)
  const reduceMotion = useReducedMotion()
  const mobile = useIsMobile()
  const narrowDrawer = useIsMobile(640)
  const {
    models,
    modelsLoaded,
    selectedModel,
    selectedThinking,
    sessions,
    updateSessionTitle,
    setThreadSelection,
    debugViewer,
    setDebugViewer,
  } = useAppShell()

  // This thread's own model and thinking knob, saved on the session the moment
  // they are picked. The shell's draft only covers the window before the
  // session's own row arrives, and the sessions that never picked.
  const selection = useSessionSelection(sessionId, models, {
    model: selectedModel,
    thinking: selectedThinking,
  })

  // Publish the selection while this thread is on screen so the global command
  // drawer edits it rather than the shell's draft, and retract it on unmount so
  // leaving the thread falls back cleanly.
  React.useEffect(() => {
    setThreadSelection({
      model: selection.model,
      thinking: selection.thinking,
      selectModel: selection.selectModel,
      selectThinking: selection.selectThinking,
      error: selection.error,
    })
    return () => setThreadSelection(null)
  }, [
    selection.model,
    selection.thinking,
    selection.selectModel,
    selection.selectThinking,
    selection.error,
    setThreadSelection,
  ])

  // Fork/multi-thread support is out of scope — this page always follows the
  // session's root thread.
  const [threadId, setThreadId] = React.useState<Uuid | null>(null)
  const [threadError, setThreadError] = React.useState<string | null>(null)

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listThreads(sessionId)
      .then((threads) => {
        if (cancelled) return
        const root = threads.find((thread) => thread.parent_id === null) ?? threads[0] ?? null
        if (!root) {
          setThreadError("This thread has no root — it may have been deleted.")
          return
        }
        setThreadId(root.id)
      })
      .catch((err) => {
        if (cancelled) return
        setThreadError(err instanceof FaberError ? err.message : "failed to load the thread")
      })

    return () => {
      cancelled = true
    }
  }, [sessionId])

  // What `@` can name. Loaded once per session: the list changes when the user
  // registers a host or a container, which is a different page, and a picker
  // that refetched on every keystroke would be answering a question nobody
  // asked between two of them.
  //
  // A thread in a project reaches environments through the project's plugins,
  // and its runs ignore tags, so it offers none rather than ones that do
  // nothing.
  const inProject = sessions.some(
    (session) => session.id === sessionId && session.project_id !== null,
  )
  const [environments, setEnvironments] = React.useState<EnvironmentCandidate[]>([])

  React.useEffect(() => {
    if (inProject) return
    let cancelled = false

    void faber
      .listEnvironments()
      .then((found) => {
        if (!cancelled) setEnvironments(found)
      })
      // Deliberately silent. Tagging is an addition to writing a message, and
      // failing to load the picker must not look like failing to send.
      .catch(() => undefined)

    return () => {
      cancelled = true
    }
  }, [sessionId, inProject])

  const mentions = React.useMemo<MentionOption[]>(
    () =>
      (inProject ? [] : environments).map((environment) => ({
        label: environment.label,
        hint:
          environment.kind === "container"
            ? `container on ${environment.host_name} · ${environment.root_path}`
            : `${environment.host_name} · ${environment.root_path}`,
        disabled: environment.disabled,
      })),
    [environments, inProject],
  )

  // The session's own row: the shell's listing leaves out sessions an agent
  // spawned, and a spawned one needs its parent to lead back to.
  const [session, setSession] = React.useState<Session | null>(null)
  React.useEffect(() => {
    let cancelled = false
    void faber
      .getSession(sessionId)
      .then((found) => {
        if (!cancelled) setSession(found)
      })
      // The thread still works without it; only the way back is missing.
      .catch(() => undefined)
    return () => {
      cancelled = true
    }
  }, [sessionId])

  // Only a project session no agent spawned can spawn agents of its own.
  const spawns = session !== null && session.project_id !== null && !session.parent_session_id
  const agents = useSessionAgents(spawns ? sessionId : null)

  const handleSessionTitle = React.useCallback(
    (title: string) => updateSessionTitle(sessionId, title),
    [sessionId, updateSessionTitle],
  )
  const { turns, loading, error, isRunning, runningRunId, streamedChars } =
    useSessionTranscript(sessionId, threadId, handleSessionTitle)

  // What the floating card shows. Cost needs the models list — an alias on a
  // message is priced against the model it names, and a thread can span more
  // than one. "Last turn" is the newest run *that reported usage*, not the
  // newest run outright: a run's usage only lands when its message completes,
  // so keying off the literal last run would blank the card the instant a new
  // message is sent, until the reply closed.
  const allUsage = React.useMemo(() => summarizeUsage(turns, models), [turns, models])
  const lastUsage = React.useMemo(() => {
    const latest = turns.filter((turn) => turn.usage).at(-1)
    return summarizeUsage(latest ? [latest] : [], models)
  }, [turns, models])

  // What autoscroll counts as something new: a turn, or a row inside one —
  // reasoning starting, a tool being called, the reply beginning. Not the
  // tokens filling those rows in, which grow the page continuously and would
  // have the view chasing every frame of a stream nobody asked to follow that
  // closely.
  const rows = React.useMemo(
    () => turns.reduce((count, turn) => count + turn.items.length, turns.length),
    [turns],
  )

  const { ref: scrollRef, onScroll, stick } = useStickToBottom<HTMLDivElement>(rows)

  // Autoscroll pins to the bottom; this is what puts the newest block at the
  // middle of the view once it gets there.
  const tailRef = useCenteredTail<HTMLDivElement>(scrollRef, rows)

  const [sendError, setSendError] = React.useState<string | null>(null)
  const noModels = modelsLoaded && models.length === 0


  // The API refuses a thread with no reply to summarize; offering it there
  // would only be a way to get that refusal.
  const hasConversation = turns.some((turn) => turn.items.length > 0)
  const canCompact = threadId !== null && !isRunning && hasConversation

  const [compactOpen, setCompactOpen] = React.useState(false)
  // Bumped per opening, so the dialog starts with empty instructions.
  const [compactKey, setCompactKey] = React.useState(0)
  const [compactError, setCompactError] = React.useState<string | null>(null)

  const openCompact = React.useCallback(() => {
    setCompactError(null)
    setCompactKey((key) => key + 1)
    setCompactOpen(true)
  }, [])

  /** Resolves to whether the compaction started; the reason it didn't is in `compactError`. */
  const handleCompact = React.useCallback(
    async (instructions: string) => {
      setCompactError(null)
      if (!threadId) return false
      try {
        await faber.compactSession(sessionId, {
          thread_id: threadId,
          ...(instructions ? { instructions } : {}),
        })
        stick()
        return true
      } catch (err) {
        setCompactError(err instanceof FaberError ? err.message : "failed to compact the conversation")
        return false
      }
    },
    [sessionId, threadId, stick],
  )

  // One line under the prompt box, whichever went wrong. The compact dialog
  // shows its own error while it is open.
  const footerError = sendError ?? (compactOpen ? null : compactError) ?? selection.error

  const handleSend = React.useCallback(
    async (content: string) => {
      setSendError(null)
      setCompactError(null)

      const command = COMPACT_COMMAND.exec(content.trim())
      if (command) return handleCompact(command[1]?.trim() ?? "")

      const model = selection.model?.alias
      if (!model || !threadId) {
        setSendError("Add a model before sending a message.")
        return false
      }

      try {
        // Sent alongside the message rather than relied on from the row: a
        // session that never picked (or whose pick failed to save) still runs
        // on what the footer is showing, and is left holding it afterwards.
        await faber.sendMessage(sessionId, {
          content,
          model,
          ...(selection.thinking ? { thinking_effort: selection.thinking } : {}),
          thread_id: threadId,
        })
        stick()
        return true
      } catch (err) {
        setSendError(err instanceof FaberError ? err.message : "failed to send the message")
        return false
      }
    },
    [sessionId, threadId, selection.model, selection.thinking, stick, handleCompact],
  )

  const handleInterrupt = React.useCallback(async () => {
    if (!runningRunId) return
    setSendError(null)

    try {
      await faber.interruptRun(runningRunId)
    } catch (err) {
      // A run that finished on its own in the moment before the click is not
      // something the user did wrong, and the stream is about to say so
      // anyway. Anything else is worth showing: the run is still going, and
      // they need to know their stop did not land.
      if (err instanceof FaberError && err.status === 409) return
      setSendError(err instanceof FaberError ? err.message : "failed to stop the run")
    }
  }, [runningRunId])

  const handleRetry = React.useCallback(
    async (runId: string, mode: RetryMode) => {
      setSendError(null)
      try {
        await faber.retryRun(runId, { mode })
        stick()
      } catch (err) {
        setSendError(err instanceof FaberError ? err.message : "failed to retry the run")
      }
    },
    [stick],
  )

  const summary = (
    <div className="p-4">
      <Collapsible open={usageOpen} onOpenChange={setUsageOpen} asChild>
        <section aria-label="Usage">
          <CollapsibleTrigger fullWidth>Usage</CollapsibleTrigger>
          <CollapsibleContent>
            {allUsage.reported ? <UsageCard all={allUsage} last={lastUsage} inline /> : <p className="text-xs text-muted-foreground">No usage reported yet.</p>}
          </CollapsibleContent>
        </section>
      </Collapsible>
    </div>
  )

  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="flex h-12 shrink-0 items-center justify-between gap-3 border-b px-4">
        <span className="truncate text-sm font-medium">{session?.title || "Thread"}</span>
        <Button size="icon" variant="ghost" aria-label="Thread summary" aria-expanded={summaryOpen} onClick={() => setSummaryOpen((open) => !open)} className="relative shrink-0">
          <PanelRight className="size-4" />
        </Button>
      </header>
      <div className="flex min-h-0 flex-1">
      <div className="relative flex min-w-0 flex-1 flex-col">
      <AgentsPanel
        sessionId={sessionId}
        agents={agents}
        models={models}
        className="absolute top-4 right-4 z-20 w-80 max-w-[calc(100%-2rem)]"
      />
      {/* Scroll anchoring is off because the tail already holds the view
          still: it shrinks by whatever the newest row grows. The browser's
          anchoring would compensate for the growth first, then get clamped
          back once the tail shrinks, so every new line of a streaming reply
          bounced the whole transcript by a line height. */}
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="min-h-0 flex-1 overflow-y-auto [overflow-anchor:none]"
      >
        <div className="mx-auto flex w-full max-w-4xl flex-col gap-8 px-4 pt-8 pb-40">
          {session?.parent_session_id ? (
            <Link
              to="/session/$sessionId"
              params={{ sessionId: session.parent_session_id }}
              data-thread-block
              className="flex w-fit items-center gap-1.5 rounded-full border px-3 py-1 text-xs text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-3.5" />
              Back to the thread that started
              <Bot className="size-3.5" />
              <span className="font-medium text-foreground">
                {session.agent_name ?? session.agent_type ?? "this agent"}
              </span>
            </Link>
          ) : null}
          {threadError ? <p className="text-sm text-destructive">{threadError}</p> : null}
          {loading && !threadError ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : null}
          <SessionAgentsContext.Provider value={agents}>
            {turns.map((turn, index) => (
              <TurnView
                key={turn.runId}
                turn={turn}
                isLast={index === turns.length - 1}
                onRetry={handleRetry}
              />
            ))}
          </SessionAgentsContext.Provider>
          {error ? <p data-thread-block className="text-sm text-destructive">{error}</p> : null}

          {/* Room under the transcript for the newest block to rise to the
              middle. `-mt-8` cancels the column gap, so at zero height it
              costs nothing. */}
          <div ref={tailRef} aria-hidden className="-mt-8 shrink-0" />
        </div>
      </div>

      <div className="pointer-events-none absolute inset-x-0 bottom-0 flex flex-col items-center gap-2 px-4 pb-6">
        {footerError ? (
          <p className="pointer-events-none max-w-4xl text-center text-xs text-destructive">
            {footerError}
          </p>
        ) : null}
        <PromptBox
          className="pointer-events-auto w-full max-w-4xl"
          autoFocus
          placeholder={noModels ? "Add a model to start chatting…" : "Message…"}
          sendDisabled={noModels || !threadId}
          isExecuting={isRunning}
          streamedChars={streamedChars}
          onSend={handleSend}
          onInterrupt={handleInterrupt}
          mentions={mentions}
          models={models}
          selectedModel={selection.model}
          onModelSelect={selection.selectModel}
          modelsLoaded={modelsLoaded}
          selectedThinking={selection.thinking}
          onThinkingSelect={selection.selectThinking}
          footerActions={
            <Button
              size="icon"
              variant="ghost"
              disabled={!canCompact}
              onClick={openCompact}
              aria-label="Compact conversation"
              title="Compact conversation"
            >
              <FoldVertical className="size-4" />
            </Button>
          }
        />
      </div>

      <CompactDialog
        key={compactKey}
        open={compactOpen}
        onOpenChange={setCompactOpen}
        onCompact={handleCompact}
        error={compactError}
      />

      {/* Scoped to this container rather than portalled to the body, so the
          debug overlay covers the conversation viewport and leaves the sidebar
          alone. AnimatePresence keeps it mounted through its exit animation. */}
      <AnimatePresence>
        {debugViewer ? (
          <DebugViewerDialog
            key="debug-viewer"
            view={debugViewer}
            threadId={threadId}
            onViewChange={setDebugViewer}
            onClose={() => setDebugViewer(null)}
          />
        ) : null}
      </AnimatePresence>
      </div>
      </div>
      </div>
      <AnimatePresence initial={false}>
      {summaryOpen && !mobile ? (
        <motion.aside
          key="summary"
          aria-label="Thread summary"
          initial={{ width: 0, opacity: 0 }}
          animate={{ width: 320, opacity: 1 }}
          exit={{ width: 0, opacity: 0 }}
          transition={{ duration: reduceMotion ? 0 : 0.24, ease: DEFAULT_EASE }}
          className="shrink-0 overflow-hidden border-l bg-background"
        >
          <div className="h-full w-80 overflow-y-auto">
          <div className="flex h-12 items-center justify-between border-b px-4">
            <h2 className="text-sm font-medium">Summary</h2>
            <Button size="icon" variant="ghost" aria-label="Close summary" onClick={() => setSummaryOpen(false)}><X className="size-4" /></Button>
          </div>
          {summary}
          </div>
        </motion.aside>
      ) : null}
      </AnimatePresence>
      {mobile ? <Dialog open={summaryOpen} onOpenChange={setSummaryOpen} direction="right">
        <DialogContent showCloseButton={false} drawerContentClassName="inset-y-0 right-0 left-auto h-dvh max-h-dvh w-[min(20rem,100vw)] rounded-none" className={narrowDrawer ? "p-0" : "inset-y-0 right-0 left-auto h-dvh max-h-dvh w-[min(20rem,100vw)] translate-x-0 translate-y-0 overflow-y-auto rounded-none p-0"}>
          <div className="flex h-12 shrink-0 items-center justify-between border-b px-4"><DialogTitle className="text-sm font-medium">Summary</DialogTitle><DialogDescription className="sr-only">Detailed usage for this thread.</DialogDescription><Button size="icon" variant="ghost" aria-label="Close summary" onClick={() => setSummaryOpen(false)}><X className="size-4" /></Button></div>
          {summary}
        </DialogContent>
      </Dialog> : null}
    </div>
  )
}
