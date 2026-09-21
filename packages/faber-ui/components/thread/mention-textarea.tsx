"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

export type MentionOption = {
  /** What gets inserted after the `@`. */
  label: string
  /** A line of context under the label — what this name actually is. */
  hint?: string
  /** Offered but greyed: a name missing from the list looks like one that
   *  does not exist. */
  disabled?: boolean
}

export type MentionTextareaProps = {
  value: string
  onValueChange: (value: string) => void
  options: MentionOption[]
  onKeyDown?: (event: React.KeyboardEvent<HTMLTextAreaElement>) => void
  placeholder?: string
  disabled?: boolean
  className?: string
  /** True while the picker is open, so the caller can hold Enter back. */
  onOpenChange?: (open: boolean) => void
  /**
   * Focus the textarea on mount, and again if it mounts disabled and is later
   * enabled. A caller that arrives at this box by navigating to it wants the
   * caret already in it.
   */
  autoFocus?: boolean
}

/**
 * The characters a name may contain, matching the server's own parser
 * (`crates/api/src/environments.rs`). The two have to agree: a client that
 * highlights `@build-2` while the server tags `@build` shows the user one
 * thing and does another.
 */
const NAME = /[\p{L}\p{N}._/-]/u

/** The `@name` being typed at the caret, or null. */
type ActiveMention = { start: number; query: string }

function mentionAt(value: string, caret: number): ActiveMention | null {
  let index = caret - 1
  while (index >= 0 && NAME.test(value[index])) index -= 1
  if (index < 0 || value[index] !== "@") return null
  // An `@` inside a word is an email address, not a tag — the same rule the
  // server applies, for the same reason.
  if (index > 0 && NAME.test(value[index - 1])) return null
  return { start: index, query: value.slice(index + 1, caret) }
}

/**
 * Splits text into plain runs and mentions, for the highlight layer.
 *
 * Only names that actually resolve are highlighted. An `@` in prose stays
 * prose, which is exactly what the server will do with it.
 */
function segments(value: string, known: Set<string>) {
  const parts: { text: string; mention: boolean }[] = []
  let plain = ""
  let index = 0

  while (index < value.length) {
    const isBoundary = index === 0 || !NAME.test(value[index - 1])
    if (value[index] === "@" && isBoundary) {
      let end = index + 1
      while (end < value.length && NAME.test(value[end])) end += 1
      const name = value.slice(index + 1, end).replace(/\.+$/, "")
      if (name && known.has(name)) {
        if (plain) parts.push({ text: plain, mention: false })
        plain = ""
        parts.push({ text: `@${name}`, mention: true })
        index = index + 1 + name.length
        continue
      }
    }
    plain += value[index]
    index += 1
  }
  if (plain) parts.push({ text: plain, mention: false })
  return parts
}

/** Font and box metrics the highlight layer and the textarea must share. */
const TEXT_BOX =
  "min-h-[56px] max-h-[220px] w-full px-4 py-4 text-[15px] leading-relaxed whitespace-pre-wrap break-words"

/**
 * A textarea that tags environments.
 *
 * Two things sit on top of an ordinary textarea, and neither changes what the
 * message *is*: the value stays plain text, `@name` included, because that is
 * what the model reads and what the server parses. A mention is a special
 * block on screen and nowhere else.
 */
export function MentionTextarea({
  value,
  onValueChange,
  options,
  onKeyDown,
  placeholder,
  disabled = false,
  className,
  onOpenChange,
  autoFocus = false,
}: MentionTextareaProps) {
  const textareaRef = React.useRef<HTMLTextAreaElement>(null)
  const highlightRef = React.useRef<HTMLDivElement>(null)
  const [active, setActive] = React.useState<ActiveMention | null>(null)
  const [highlighted, setHighlighted] = React.useState(0)

  React.useEffect(() => {
    if (autoFocus && !disabled) textareaRef.current?.focus()
  }, [autoFocus, disabled])

  const known = React.useMemo(
    () => new Set(options.map((option) => option.label)),
    [options],
  )

  const matches = React.useMemo(() => {
    if (!active) return []
    const query = active.query.toLowerCase()
    return options
      .filter((option) => option.label.toLowerCase().includes(query))
      .slice(0, 8)
  }, [active, options])

  const open = active !== null && matches.length > 0

  React.useEffect(() => {
    onOpenChange?.(open)
  }, [open, onOpenChange])

  /**
   * Recomputes the mention under the caret, and starts the list at the top.
   *
   * The reset belongs here rather than in an effect on the query: the two
   * pieces of state change for one reason — the caret moved — and splitting
   * them across a render costs a cascading one to put them back together.
   */
  const syncMention = React.useCallback((element: HTMLTextAreaElement) => {
    setActive(mentionAt(element.value, element.selectionStart ?? 0))
    setHighlighted(0)
  }, [])

  const resize = React.useCallback((element: HTMLTextAreaElement) => {
    element.style.height = "auto"
    element.style.height = `${Math.min(element.scrollHeight, 220)}px`
  }, [])

  const choose = React.useCallback(
    (option: MentionOption) => {
      if (!active || option.disabled) return
      const element = textareaRef.current
      if (!element) return

      const caret = element.selectionStart ?? value.length
      const next = `${value.slice(0, active.start)}@${option.label} ${value.slice(caret)}`
      const at = active.start + option.label.length + 2

      onValueChange(next)
      setActive(null)
      // After React has written the new value, or the caret lands against the
      // old one and the user types into the middle of the name they just
      // picked.
      requestAnimationFrame(() => {
        const target = textareaRef.current
        if (!target) return
        target.focus()
        target.setSelectionRange(at, at)
        resize(target)
      })
    },
    [active, onValueChange, resize, value],
  )

  const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (open) {
      if (event.key === "ArrowDown") {
        event.preventDefault()
        setHighlighted((current) => (current + 1) % matches.length)
        return
      }
      if (event.key === "ArrowUp") {
        event.preventDefault()
        setHighlighted((current) => (current - 1 + matches.length) % matches.length)
        return
      }
      // Enter picks rather than sends while the list is open — the same key
      // doing both would send a half-typed name every time.
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault()
        choose(matches[highlighted])
        return
      }
      if (event.key === "Escape") {
        event.preventDefault()
        setActive(null)
        return
      }
    }
    onKeyDown?.(event)
  }

  return (
    <div className="relative">
      {/* Behind the textarea, mirroring it exactly. The textarea's own text is
          transparent, so what is read is this layer — which is how a mention
          gets a background without the value ever being anything but text. */}
      <div
        ref={highlightRef}
        aria-hidden
        className={cn(TEXT_BOX, "pointer-events-none absolute inset-0 overflow-hidden text-foreground")}
      >
        {segments(value, known).map((part, index) =>
          part.mention ? (
            <span
              key={index}
              className="rounded-md bg-primary/12 px-0.5 font-medium text-primary"
            >
              {part.text}
            </span>
          ) : (
            <span key={index}>{part.text}</span>
          ),
        )}
        {/* A trailing newline collapses without something after it, and the
            layer then scrolls one line behind the textarea. */}
        {"​"}
      </div>

      <textarea
        ref={textareaRef}
        value={value}
        placeholder={placeholder}
        autoComplete="off"
        disabled={disabled}
        rows={1}
        className={cn(
          TEXT_BOX,
          "relative resize-none overflow-y-auto bg-transparent text-transparent caret-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50",
          className,
        )}
        onChange={(event) => {
          onValueChange(event.target.value)
          syncMention(event.currentTarget)
        }}
        onScroll={(event) => {
          const layer = highlightRef.current
          if (layer) layer.scrollTop = event.currentTarget.scrollTop
        }}
        onClick={(event) => syncMention(event.currentTarget)}
        onKeyUp={(event) => {
          // Arrowing through the list moves the caret not at all and the
          // highlight very much; re-syncing here would put it back on the
          // first row after every keypress.
          if (open && (event.key === "ArrowUp" || event.key === "ArrowDown")) return
          syncMention(event.currentTarget)
        }}
        onBlur={() => setActive(null)}
        onInput={(event) => resize(event.currentTarget)}
        onKeyDown={handleKeyDown}
      />

      {open ? (
        <div className="absolute bottom-full left-2 z-20 mb-2 w-72 overflow-hidden rounded-lg border border-input bg-popover shadow-lg">
          <p className="border-b border-border/60 px-3 py-1.5 text-[11px] uppercase tracking-wide text-muted-foreground">
            Environments
          </p>
          <ul className="max-h-56 overflow-y-auto py-1">
            {matches.map((option, index) => (
              <li key={option.label}>
                <button
                  type="button"
                  // `onMouseDown`, not `onClick`: the textarea's blur would
                  // close the list before a click ever landed.
                  onMouseDown={(event) => {
                    event.preventDefault()
                    choose(option)
                  }}
                  onMouseEnter={() => setHighlighted(index)}
                  disabled={option.disabled}
                  className={cn(
                    "flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left text-sm",
                    index === highlighted && "bg-accent",
                    option.disabled && "opacity-50",
                  )}
                >
                  <span className="font-medium">@{option.label}</span>
                  {option.hint ? (
                    <span className="text-xs text-muted-foreground">{option.hint}</span>
                  ) : null}
                </button>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  )
}
