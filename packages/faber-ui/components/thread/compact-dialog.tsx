"use client"

import * as React from "react"

import { Button } from "@charlotte-ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@charlotte-ui/dialog"
import { Textarea } from "@charlotte-ui/textarea"

/** The API refuses longer ones (`MAX_COMPACT_INSTRUCTIONS` in `crates/api`). */
export const MAX_COMPACT_INSTRUCTIONS = 2000

/**
 * Asks to compact the thread now, with what the summary should be sure to
 * keep. Resolving `onCompact` to `false` keeps the dialog open; the caller
 * shows why.
 *
 * Mount it keyed on each opening so the instructions start empty.
 */
export function CompactDialog({
  open,
  onOpenChange,
  onCompact,
  error,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onCompact: (instructions: string) => Promise<boolean>
  error?: string | null
}) {
  const [instructions, setInstructions] = React.useState("")
  const [submitting, setSubmitting] = React.useState(false)
  const tooLong = [...instructions.trim()].length > MAX_COMPACT_INSTRUCTIONS

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (tooLong) return
    setSubmitting(true)
    try {
      if (await onCompact(instructions.trim())) onOpenChange(false)
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>Compact conversation</DialogTitle>
            <DialogDescription>
              The model summarizes the conversation so far, and goes on from the summary instead. The
              messages stay on screen, but the model will no longer see them.
            </DialogDescription>
          </DialogHeader>

          <div className="flex flex-col gap-1.5">
            <label htmlFor="compact-instructions" className="text-sm font-medium">
              What to keep <span className="font-normal text-muted-foreground">(optional)</span>
            </label>
            <Textarea
              id="compact-instructions"
              value={instructions}
              onChange={(event) => setInstructions(event.target.value)}
              placeholder="e.g. the exact error messages, and the API we settled on"
              rows={3}
              autoFocus
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault()
                  event.currentTarget.form?.requestSubmit()
                }
              }}
            />
            {tooLong ? (
              <p className="text-xs text-destructive">
                Keep it to {MAX_COMPACT_INSTRUCTIONS} characters or fewer.
              </p>
            ) : null}
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={tooLong} loading={submitting} loadingText="Compacting">
              Compact
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
