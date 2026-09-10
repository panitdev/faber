import { Boxes } from "lucide-react"
import * as React from "react"

import { FaberIndicator } from "@/components/thread/faber-indicator"
import { Markdown } from "@/components/thread/markdown"
import { AgentMessage, AgentRun, AgentStep, AgentThinking } from "@/components/ui/agent-run"
import { toolDisplay } from "@/lib/thread/tools"
import type { ContentBlock, Turn } from "@/lib/thread/transcript"
import { useThinkingModes } from "@/lib/thread/use-thinking-modes"

function userText(turn: Turn): string {
  return turn.userContent
    .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
    .map((block) => block.text)
    .join("\n\n")
}

// Timeline gutter shared by the transcript (`AgentRun`) and the tail
// indicator — both centre on `nodeSize / 2`, so they must use the same value.
const TIMELINE_NODE_SIZE = 40

/** One user turn plus the agent's run in response, on the session timeline. */
export function TurnView({ turn, isLast = false }: { turn: Turn; isLast?: boolean }) {
  const text = userText(turn)
  const thinking = useThinkingModes()

  // Rows this turn already had when it mounted are HISTORY, not an entrance:
  // a restored session, another conversation switched to, or a reload part way
  // through a run. A reveal batch costs the SUM of its rows' durations, so
  // letting them draw spends seconds redrawing a run the user has already seen.
  // A turn opens with no items and gets them as they stream, so a live turn
  // reads `false` here and still draws itself — as do rows arriving later,
  // either way.
  const [restored] = React.useState(() => turn.items.length > 0)

  return (
    <div className="flex flex-col gap-6">
      {text ? (
        <div
          data-thread-block
          className="ml-auto max-w-[85%] rounded-2xl bg-muted px-4 py-2.5 text-[15px] leading-relaxed text-foreground"
        >
          <Markdown text={text} softBreaks />
        </div>
      ) : null}

      {/* Neither side's words: the session saying an environment was added.
          Centered and quiet, so it reads as a change to the conversation
          rather than as something someone said in it. */}
      {turn.notices.map((notice, index) => (
        <p
          key={`${turn.runId}:notice:${index}`}
          data-thread-block
          className="mx-auto flex items-center gap-1.5 text-xs text-muted-foreground"
        >
          <Boxes className="size-3.5" />
          {notice}
        </p>
      ))}

      {/* Rows are composed by hand rather than passed as `items`, because the
          data-driven path renders a message's text as a plain string — the only
          way to hand it to Markdown is to build the row ourselves. `isLast` is
          still AgentRun's to inject.

          `thread-rows` styles nothing — it is how autoscroll finds the rows of
          a run, which it aims at one at a time. See `useCenteredTail`. */}
      {turn.items.length > 0 ? (
        <AgentRun
          className="thread-rows"
          revealOnMount={!restored}
          // Tighter than the registry default (64/2) — see git history for
          // "tighten agent run ring". Overridden here, not in the registry
          // file, so a future `agent-run` sync can't silently drop it.
          nodeSize={TIMELINE_NODE_SIZE}
          lineWidth={1.5}
        >
          {turn.items.map((item) => {
            if (item.kind === "message") {
              return (
                <AgentMessage key={item.id}>
                  <Markdown text={item.text} />
                </AgentMessage>
              )
            }
            if (item.kind === "thinking") {
              const mode = thinking.modeOf(item.id, item.streaming)
              return (
                // Plain text, not Markdown: `peek` sizes its window in lines of
                // this body's own leading, which a block element's margins
                // would throw off.
                <AgentThinking
                  key={item.id}
                  mode={mode}
                  onModeChange={() => thinking.toggle(item.id, mode)}
                >
                  <p className="whitespace-pre-wrap">{item.text}</p>
                </AgentThinking>
              )
            }
            const tool = toolDisplay(item.name, item.input)
            return (
              <AgentStep key={item.id} state={item.state} title={tool.title} meta={tool.meta}>
                {item.result}
              </AgentStep>
            )
          })}
        </AgentRun>
      ) : null}

      {/* The indicator is the tail of the timeline, so only the last turn — the
          one that can still be running — carries it. Not a block autoscroll
          aims at: it trails whatever just arrived, and centring it would push
          the row the user came to read above the middle. */}
      {isLast ? (
        <FaberIndicator working={turn.status === "running"} nodeSize={TIMELINE_NODE_SIZE} />
      ) : null}

      {turn.status === "error" ? (
        <p data-thread-block className="text-sm text-destructive">
          {turn.errorMessage ?? "The run failed."}
        </p>
      ) : null}

      {/* Muted, not destructive: the user stopped this themselves and already
          knows why. The note is here so the reply reads as cut short on
          purpose rather than as one that simply trailed off. */}
      {turn.status === "interrupted" ? (
        <p data-thread-block className="text-sm text-muted-foreground">
          Stopped.
        </p>
      ) : null}
    </div>
  )
}
