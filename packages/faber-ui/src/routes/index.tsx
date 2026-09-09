import * as React from "react"
import { createFileRoute, useNavigate } from "@tanstack/react-router"

import { faber, FaberError } from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import { PromptBox } from "@/components/thread/prompt-box"
import { ModelPicker } from "@/components/thread/model-picker"
import { ThinkingPicker } from "@/components/thread/thinking-picker"
import { thinkingOf } from "@/lib/models/thinking"

export const Route = createFileRoute("/")({ component: Home })

function Home() {
  const navigate = useNavigate()
  const {
    models,
    modelsLoaded,
    selectedModel,
    selectModel,
    selectedThinking,
    selectThinking,
    createSession,
  } = useAppShell()
  const [sendError, setSendError] = React.useState<string | null>(null)
  const [sending, setSending] = React.useState(false)

  const noModels = modelsLoaded && models.length === 0

  const handleSend = React.useCallback(
    async (content: string) => {
      setSendError(null)

      const model = selectedModel?.alias
      if (!model) {
        setSendError("Add a model before starting a thread.")
        return false
      }

      setSending(true)
      try {
        const created = await createSession()
        if (!created) return false

        // Both settings travel with the first message: the session did not
        // exist when they were picked, and this is what leaves it holding
        // them.
        await faber.sendMessage(created.id, {
          content,
          model,
          ...(selectedThinking ? { thinking_effort: selectedThinking } : {}),
        })
        navigate({ to: "/session/$sessionId", params: { sessionId: created.id } })
        return true
      } catch (err) {
        setSendError(err instanceof FaberError ? err.message : "failed to send the message")
        return false
      } finally {
        setSending(false)
      }
    },
    [selectedModel, selectedThinking, createSession, navigate],
  )

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 p-4">
      <PromptBox
        className="w-full max-w-4xl"
        placeholder={noModels ? "Add a model to start chatting…" : "Start a thread…"}
        sendDisabled={noModels || sending}
        onSend={handleSend}
        footerActions={
          <>
            <ModelPicker
              models={models}
              selected={selectedModel}
              loaded={modelsLoaded}
              onSelect={selectModel}
            />
            <ThinkingPicker
              capability={thinkingOf(selectedModel)}
              selected={selectedThinking}
              onSelect={selectThinking}
            />
          </>
        }
      />
      {sendError ? (
        <p className="w-full max-w-4xl text-sm text-destructive">{sendError}</p>
      ) : null}
    </div>
  )
}
