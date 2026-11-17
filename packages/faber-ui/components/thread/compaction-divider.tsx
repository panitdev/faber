import { ChevronRight, FoldVertical, Loader2 } from "lucide-react"
import * as React from "react"

import { cn } from "@charlotte-ui/utils"
import { Markdown } from "@/components/thread/markdown"
import { formatTokens } from "@/components/thread/usage-card"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import type { Compaction } from "@/lib/thread/transcript"

function label(compaction: Compaction): string {
  const automatic = compaction.trigger === "automatic"
  switch (compaction.status) {
    case "running":
      return automatic ? "Context is nearly full — compacting the conversation…" : "Compacting the conversation…"
    case "failed":
      return automatic ? "Couldn't compact the conversation; carrying on as it was" : "Compaction didn't finish"
    case "done":
      return automatic ? "Conversation compacted automatically" : "Conversation compacted"
  }
}

/** "48.2k tokens · 31 messages", from whatever the event reported. */
function detail(compaction: Compaction): string | null {
  const parts: string[] = []
  if (compaction.tokensBefore !== undefined) parts.push(`${formatTokens(compaction.tokensBefore)} tokens`)
  if (compaction.summarized !== undefined) {
    parts.push(`${compaction.summarized} message${compaction.summarized === 1 ? "" : "s"}`)
  }
  return parts.length > 0 ? `${parts.join(" · ")} summarized` : null
}

/**
 * Where the conversation was replaced by its summary. Everything above the
 * line is still on screen, but the model no longer sees it — only the
 * summary, which opens underneath.
 *
 * A rule across the column with the note in its middle, the way a change to
 * the conversation reads apart from anything said in it.
 */
export function CompactionDivider({ compaction }: { compaction: Compaction }) {
  const [open, setOpen] = React.useState(false)
  const summary = compaction.status === "done" ? compaction.summary?.trim() : undefined
  const info = compaction.status === "done" ? detail(compaction) : compaction.error

  const note = (
    <>
      {compaction.status === "running" ? (
        <Loader2 className="size-3.5 shrink-0 animate-spin" />
      ) : (
        <FoldVertical className="size-3.5 shrink-0" />
      )}
      <span className="truncate">{label(compaction)}</span>
      {summary ? (
        <ChevronRight className={cn("size-3.5 shrink-0 transition-transform", open && "rotate-90")} />
      ) : null}
    </>
  )

  return (
    <Collapsible open={open} onOpenChange={setOpen} data-thread-block className="flex flex-col gap-3">
      <div className="flex items-center gap-3 text-xs text-muted-foreground">
        <span aria-hidden className="h-px flex-1 bg-border" />
        {summary ? (
          <CollapsibleTrigger
            className="flex min-w-0 items-center gap-1.5 rounded-full px-2 py-0.5 hover:text-foreground"
            aria-label={open ? "Hide the summary" : "Show the summary"}
          >
            {note}
          </CollapsibleTrigger>
        ) : (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1.5 px-2 py-0.5",
              compaction.status === "failed" && compaction.trigger === "requested" && "text-destructive",
            )}
          >
            {note}
          </span>
        )}
        <span aria-hidden className="h-px flex-1 bg-border" />
      </div>
      {info ? <p className="-mt-2 text-center text-[11px] text-muted-foreground/80">{info}</p> : null}
      {summary ? (
        <CollapsibleContent>
          <div className="rounded-2xl border bg-muted/40 px-4 py-3 text-sm leading-relaxed text-foreground">
            <Markdown text={summary} />
          </div>
        </CollapsibleContent>
      ) : null}
    </Collapsible>
  )
}
