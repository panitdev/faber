import * as React from "react"

import {
  faber,
  FaberError,
  type EnvironmentCandidate,
  type Uuid,
} from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import { PromptBox } from "@/components/thread/prompt-box"
import { ModelPicker } from "@/components/thread/model-picker"
import { ThinkingPicker } from "@/components/thread/thinking-picker"
import { thinkingOf } from "@/lib/models/thinking"
import { useSessionSelection } from "@/lib/sessions/use-session-selection"
import { TurnView } from "@/components/thread/turn"
import type { MentionOption } from "@/components/thread/mention-textarea"
import { useSessionTranscript } from "@/lib/thread/use-session-transcript"
import { useStickToBottom } from "@/lib/thread/use-stick-to-bottom"
import { useCenteredTail } from "@/lib/thread/use-centered-tail"

export default function SessionClient({ sessionId }: { sessionId: string }) {
  const id = sessionId as Uuid

  // Keyed on the session id so navigating between threads remounts this
  // subtree — a fresh set of `useState` defaults rather than an effect that
  // resets state mid-life.
  return <SessionThread key={id} sessionId={id} />
}

function SessionThread({ sessionId }: { sessionId: Uuid }) {
  const {
    models,
    modelsLoaded,
    selectedModel,
    selectedThinking,
    updateSessionTitle,
  } = useAppShell()

  // This thread's own model and thinking knob, saved on the session the moment
  // they are picked. The shell's draft only covers the window before the
  // session's own row arrives, and the sessions that never picked.
  const selection = useSessionSelection(sessionId, models, {
    model: selectedModel,
    thinking: selectedThinking,
  })

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
  const [environments, setEnvironments] = React.useState<EnvironmentCandidate[]>([])

  React.useEffect(() => {
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
  }, [sessionId])

  const mentions = React.useMemo<MentionOption[]>(
    () =>
      environments.map((environment) => ({
        label: environment.label,
        hint:
          environment.kind === "container"
            ? `container on ${environment.host_name} · ${environment.root_path}`
            : `${environment.host_name} · ${environment.root_path}`,
        disabled: environment.disabled,
      })),
    [environments],
  )

  const handleSessionTitle = React.useCallback(
    (title: string) => updateSessionTitle(sessionId, title),
    [sessionId, updateSessionTitle],
  )
  const { turns, loading, error, isRunning, runningRunId, streamedChars } =
    useSessionTranscript(sessionId, threadId, handleSessionTitle)

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

  // One line under the prompt box, whichever went wrong.
  const footerError = sendError ?? selection.error

  const handleSend = React.useCallback(
    async (content: string) => {
      setSendError(null)

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
    [sessionId, threadId, selection.model, selection.thinking, stick],
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

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-4xl flex-col gap-8 px-4 pt-8 pb-40">
          {threadError ? <p className="text-sm text-destructive">{threadError}</p> : null}
          {loading && !threadError ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : null}
          {turns.map((turn, index) => (
            <TurnView key={turn.runId} turn={turn} isLast={index === turns.length - 1} />
          ))}
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
          placeholder={noModels ? "Add a model to start chatting…" : "Message…"}
          sendDisabled={noModels || !threadId}
          isExecuting={isRunning}
          streamedChars={streamedChars}
          onSend={handleSend}
          onInterrupt={handleInterrupt}
          mentions={mentions}
          footerActions={
            <>
              <ModelPicker
                models={models}
                selected={selection.model}
                loaded={modelsLoaded}
                onSelect={selection.selectModel}
              />
              <ThinkingPicker
                capability={thinkingOf(selection.model)}
                selected={selection.thinking}
                onSelect={selection.selectThinking}
              />
            </>
          }
        />
      </div>
    </div>
  )
}
