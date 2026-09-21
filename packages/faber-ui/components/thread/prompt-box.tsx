"use client"

import * as React from "react"
import { Paperclip, SendHorizontal, Square } from "lucide-react"

import { SquircleFrame } from "@/components/util/squircle-frame"

import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { ModelPicker } from "@/components/thread/model-picker"
import type { ModelConfig, ThinkingSelection } from "@/lib/api"
import {
  MentionTextarea,
  type MentionOption,
} from "@/components/thread/mention-textarea"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"

const INTERRUPT_CONFIRM_THRESHOLD = 200

export type PromptBoxProps = {
  onSend?: (message: string) => boolean | void | Promise<boolean | void>
  onInterrupt?: () => void | Promise<void>
  isExecuting?: boolean
  streamedChars?: number
  disabled?: boolean
  sendDisabled?: boolean
  placeholder?: string
  className?: string
  /** Extra controls for the footer, shown beside the attach button. */
  footerActions?: React.ReactNode
  /**
   * Focus the input on mount. Set on the surfaces a user arrives at by
   * navigating — the landing page and an open thread — so the caret is waiting
   * rather than a click away.
   */
  autoFocus?: boolean
  models?: ModelConfig[]
  selectedModel?: ModelConfig | null
  onModelSelect?: (alias: string) => void
  modelsLoaded?: boolean
  selectedThinking?: ThinkingSelection | null
  onThinkingSelect?: (selection: ThinkingSelection | null) => void
  /**
   * Environments the caller can tag with `@`. Empty means the picker never
   * opens, which is the right behaviour for a user who has registered none.
   */
  mentions?: MentionOption[]
}

export function PromptBox({
  onSend = () => true,
  onInterrupt,
  isExecuting = false,
  streamedChars = 0,
  disabled = false,
  sendDisabled = false,
  placeholder = "Type a message...",
  className,
  footerActions,
  autoFocus = false,
  models = [],
  selectedModel = null,
  onModelSelect,
  modelsLoaded = true,
  selectedThinking = null,
  onThinkingSelect,
  mentions = [],
}: PromptBoxProps) {
  const [message, setMessage] = React.useState("")
  const [confirmOpen, setConfirmOpen] = React.useState(false)
  // Enter belongs to whichever is in front. While the picker is open it takes
  // the name; otherwise it sends the message.
  const [picking, setPicking] = React.useState(false)

  const handleSubmit = async () => {
    if (!message.trim() || disabled || sendDisabled) return

    const accepted = await onSend(message)
    if (accepted === false) {
      return
    }

    setMessage("")
  }

  const handleInterrupt = () => {
    if (!onInterrupt) return
    if (streamedChars >= INTERRUPT_CONFIRM_THRESHOLD) {
      setConfirmOpen(true)
    } else {
      void onInterrupt()
    }
  }

  const handleConfirmedInterrupt = () => {
    setConfirmOpen(false)
    void onInterrupt?.()
  }

  const boxContent = (
    <>
      <MentionTextarea
        value={message}
        onValueChange={setMessage}
        options={mentions}
        onOpenChange={setPicking}
        placeholder={placeholder}
        disabled={disabled}
        autoFocus={autoFocus}
        onKeyDown={(e) => {
          if (e.key === "Escape" && isExecuting) {
            e.preventDefault()
            handleInterrupt()
            return
          }
          if (e.key === "Enter" && !e.shiftKey && !isExecuting && !picking) {
            e.preventDefault()
            void handleSubmit()
          }
        }}
      />
      <div className="flex flex-wrap items-end justify-between gap-3 px-3 pb-3">
        <div className="flex min-w-0 flex-1 items-center gap-1">
          <Button size="icon" variant="outline" disabled={disabled}>
            <Paperclip className="size-4" />
          </Button>
          {onModelSelect ? (
            <ModelPicker
              models={models}
              selected={selectedModel}
              loaded={modelsLoaded}
              onSelect={onModelSelect}
              selectedThinking={selectedThinking}
              onThinkingSelect={onThinkingSelect}
              disabled={disabled}
            />
          ) : null}
          {footerActions}
        </div>
        {isExecuting ? (
          <Button
            size="icon"
            variant="outline"
            disabled={disabled || !onInterrupt}
            onClick={handleInterrupt}
            className="transition-all"
            aria-label="Interrupt"
          >
            <Square className="size-4 fill-current" />
          </Button>
        ) : (
          <Button
            size="icon"
            disabled={disabled || sendDisabled || !message.trim()}
            onClick={() => void handleSubmit()}
            className="transition-all"
          >
            <SendHorizontal className="size-4" />
          </Button>
        )}
      </div>
    </>
  )

  return (
    <>
      <div className={cn("w-full", className)}>
        <SquircleFrame
          cornerRadius={24}
          borderWidth={1}
          cornerSmoothing={1}
          className="shadow-[0_14px_40px_rgba(0,0,0,0.06)] transition-all"
          faceClassName="bg-background/90 backdrop-blur-sm transition-all"
        >
          {boxContent}
        </SquircleFrame>
      </div>

      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Interrupt agent?</AlertDialogTitle>
            <AlertDialogDescription>
              The agent has been running for a while. Interrupting now may leave
              work incomplete.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep running</AlertDialogCancel>
            <AlertDialogAction onClick={handleConfirmedInterrupt}>
              Interrupt
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
