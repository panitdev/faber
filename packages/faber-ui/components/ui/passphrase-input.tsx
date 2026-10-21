"use client"

import * as React from "react"
import { AnimatePresence, motion } from "framer-motion"

import { cn, DEFAULT_EASE } from "@charlotte-ui/utils"

type PassphraseInputProps = {
  /** Controlled value — the whole phrase as one space-separated string. */
  value: string
  onChange: (value: string) => void
  /** Fixed number of words the phrase must contain. */
  wordCount: number
  /**
   * Opt-in word validation. When set, a word opens the next slot as soon as it
   * matches the list exactly; reaching for the next word with Space or Enter on
   * a word that doesn't match shakes and marks it instead. Leave undefined to
   * accept any word. Note that a word which is a prefix of another list entry
   * advances on its own, making the longer one untypable.
   */
  wordlist?: string[]
  /** Fired once every word slot is filled, with the full phrase. */
  onComplete?: (value: string) => void
  /** Renders the error shake and destructive styling. */
  error?: boolean
  disabled?: boolean
  autoFocus?: boolean
  /** Accessible label for the group. */
  "aria-label"?: string
  id?: string
  className?: string
}

// Split a raw phrase into exactly `count` slots, padding trailing empties.
const toSlots = (value: string, count: number) => {
  const words = value.length ? value.split(" ") : []
  return Array.from({ length: count }, (_, i) => words[i] ?? "")
}

// Which word the caret sits in = the number of spaces before it.
const wordIndexAt = (value: string, caret: number) =>
  value.slice(0, caret).split(" ").length - 1

const spring = { type: "spring" as const, stiffness: 460, damping: 32 }

export function PassphraseInput({
  value,
  onChange,
  wordCount,
  wordlist,
  onComplete,
  error = false,
  disabled = false,
  autoFocus = false,
  "aria-label": ariaLabel = "Recovery phrase",
  id,
  className,
}: PassphraseInputProps) {
  const inputRef = React.useRef<HTMLInputElement | null>(null)
  const [focused, setFocused] = React.useState(false)
  const [activeIndex, setActiveIndex] = React.useState(0)
  // The word the user tried, and failed, to advance past. Cleared as soon as
  // they start fixing it.
  const [rejectedIndex, setRejectedIndex] = React.useState<number | null>(null)
  const wasComplete = React.useRef(false)
  const pendingCaret = React.useRef<number | null>(null)

  const wordSet = React.useMemo(() => wordlist && new Set(wordlist), [wordlist])

  const words = toSlots(value, wordCount)
  const filledCount = words.filter(Boolean).length
  const isComplete = filledCount === wordCount
  // A rejected word borrows the error prop's shake — same gesture, same meaning.
  const shake = error || rejectedIndex !== null

  // Fire onComplete on the transition into a fully filled phrase.
  React.useEffect(() => {
    if (isComplete && !wasComplete.current) onComplete?.(value)
    wasComplete.current = isComplete
  }, [isComplete, value, onComplete])

  React.useEffect(() => {
    if (autoFocus) focusWord(indexOfFirstEmpty(words))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoFocus])

  // Editing anything is the user answering the complaint — drop it.
  React.useEffect(() => {
    setRejectedIndex(null)
  }, [value])

  // Read the caret back from the input and light up the word it lives in.
  const syncActive = React.useCallback(() => {
    const el = inputRef.current
    if (!el) return
    setActiveIndex(wordIndexAt(el.value, el.selectionStart ?? el.value.length))
  }, [])

  const focusWord = (index: number) => {
    const el = inputRef.current
    if (!el) return
    // Park the caret at the end of the chosen word so typing extends it.
    const wordSlots = toSlots(el.value, wordCount)
    const clamped = Math.max(0, Math.min(index, wordCount - 1))
    const caret =
      wordSlots.slice(0, clamped).join(" ").length +
      (clamped > 0 ? 1 : 0) +
      wordSlots[clamped].length
    el.focus()
    el.setSelectionRange(caret, caret)
    setActiveIndex(clamped)
  }

  // One space between words, never a leading space, capped at wordCount words.
  const normalize = (raw: string) =>
    raw
      .replace(/\s+/g, " ")
      .replace(/^ /, "")
      .split(" ")
      .slice(0, wordCount)
      .join(" ")

  const commit = (raw: string, caret: number) => {
    const next = normalize(raw)
    // With a wordlist, finishing a word opens the next slot on its own by
    // appending the same separator the Space key would have. Only while typing
    // at the tail (caret at the very end) — an interior fix shouldn't yank the
    // caret away from the word being edited.
    const here = wordIndexAt(next, next.length)
    const word = toSlots(next, wordCount)[here]
    const advance =
      !!wordSet && caret === raw.length && !!word && wordSet.has(word) && here < wordCount - 1

    onChange(advance ? `${next} ` : next)
    if (advance) {
      // The caret can only be placed once React has rendered the trailing space.
      pendingCaret.current = next.length + 1
      setActiveIndex(here + 1)
    }
    return advance
  }

  // Apply a caret position that had to wait for the new value to render.
  React.useLayoutEffect(() => {
    const caret = pendingCaret.current
    if (caret == null) return
    pendingCaret.current = null
    inputRef.current?.setSelectionRange(caret, caret)
  }, [value])

  return (
    // Ghost wrapper: a fixed-height box that never animates. It reserves the
    // tallest (focused) height up front, so the pill inside can grow and shrink
    // vertically — centered — without ever nudging the content below it.
    <motion.div
      animate={shake ? { x: [0, -6, 6, -4, 4, 0] } : { x: 0 }}
      transition={
        shake
          ? { duration: 0.4, ease: DEFAULT_EASE }
          : { duration: 0.2, ease: DEFAULT_EASE }
      }
      className={cn("flex h-[3.375rem] w-fit max-w-full items-center justify-center", className)}
    >
      {/* Root field: the visible pill. Its height is dynamic (short dot row ↔
          taller when a word field opens); it reflows as the slots below change
          size, and the ghost keeps it centered. */}
      <div
        role="group"
        aria-label={ariaLabel}
        onMouseDown={(e) => {
          if (disabled) return
          // Empty-space clicks land the caret at the first unfilled word.
          e.preventDefault()
          focusWord(indexOfFirstEmpty(words))
        }}
        className={cn(
          "relative flex w-fit max-w-full flex-wrap items-center gap-2 rounded-2xl border bg-card p-2.5 transition-[border-color,box-shadow] duration-300 ease-out",
          error
            ? "border-destructive/70 ring-4 ring-destructive/10"
            : focused
              ? "border-primary ring-4 ring-primary/10"
              : "border-border",
          disabled ? "opacity-60" : "cursor-text"
        )}
      >
      {words.map((word, index) => {
        const isActive = focused && index === activeIndex
        const filled = word.length > 0
        const isRejected = index === rejectedIndex

        return (
          <motion.div
            key={index}
            initial={false}
            animate={{ width: isActive ? "auto" : 12, height: isActive ? 32 : 12 }}
            transition={spring}
            onMouseDown={(e) => {
              if (disabled) return
              // Beat the container handler and aim the caret at this word.
              e.preventDefault()
              e.stopPropagation()
              focusWord(index)
            }}
            className={cn(
              // Animate real width/height (not a layout scale transform) so the
              // fully-rounded shape stays a crisp circle → capsule. No padding
              // here: a flex item won't shrink below its own padding, which would
              // stop the dot collapsing to a true 12px circle — the capsule's
              // padding lives on the inner content instead.
              "relative flex shrink-0 items-center justify-center overflow-hidden rounded-full transition-colors duration-300 ease-out",
              isRejected
                ? "bg-destructive/10 ring-1 ring-inset ring-destructive"
                : isActive
                  ? "bg-muted"
                  : filled
                    ? error
                      ? "bg-destructive/70"
                      : "bg-primary"
                    : "bg-transparent ring-1 ring-inset ring-border"
            )}
          >
            <AnimatePresence initial={false}>
              {isActive ? (
                <motion.div
                  key="field"
                  initial={{ opacity: 0 }}
                  animate={{ opacity: 1 }}
                  exit={{ opacity: 0 }}
                  transition={{ duration: 0.15 }}
                  className="flex items-center gap-2 px-3"
                >
                  <span className="select-none text-xs font-medium tabular-nums text-muted-foreground/70">
                    {index + 1}
                  </span>
                  <span
                    className={cn(
                      "whitespace-pre font-mono text-base leading-none",
                      isRejected ? "text-destructive" : "text-foreground"
                    )}
                  >
                    {word}
                  </span>
                  <motion.span
                    aria-hidden="true"
                    className={cn(
                      "h-5 w-[2px] rounded-full",
                      isRejected ? "bg-destructive" : "bg-primary"
                    )}
                    animate={{ opacity: [1, 1, 0, 0] }}
                    transition={{ duration: 1, repeat: Infinity, ease: "linear" }}
                  />
                </motion.div>
              ) : null}
            </AnimatePresence>
          </motion.div>
        )
      })}

      {/* One transparent input holds the whole phrase, so selection, copy, and
          paste all behave like a single native field. The morphing dots above
          are just a decorated view of its value. */}
      <input
        ref={inputRef}
        id={id}
        value={value}
        disabled={disabled}
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        autoComplete="off"
        aria-label={ariaLabel}
        onChange={(e) => {
          const el = e.currentTarget
          // An auto-advance already parked the caret and the active word.
          if (!commit(el.value, el.selectionStart ?? el.value.length)) syncActive()
        }}
        onKeyDown={(e) => {
          const el = e.currentTarget
          const caret = el.selectionStart ?? el.value.length
          const atWordEnd = caret === el.value.length || el.value[caret] === " "

          if (e.key === " " || e.key === "Enter") {
            // Reaching for the next word with one that isn't on the list is a
            // dead end. Say so, rather than swallowing the key and looking
            // broken. Checked before the Space gate below and without regard to
            // position, so the last word complains too.
            const here = wordIndexAt(el.value, caret)
            const word = toSlots(el.value, wordCount)[here]
            if (wordSet && word && !wordSet.has(word)) {
              e.preventDefault()
              setRejectedIndex(here)
              return
            }
          }

          if (e.key === " ") {
            // Space only ever separates words at a word boundary — never splits
            // a word in two and never opens an empty or extra slot. With a
            // wordlist, it also won't leave a word that isn't on it.
            const here = wordIndexAt(el.value, caret)
            const word = toSlots(el.value, wordCount)[here]
            const canAdvance =
              atWordEnd &&
              !!word &&
              (!wordSet || wordSet.has(word)) &&
              here < wordCount - 1
            if (!canAdvance) e.preventDefault()
          } else if (
            e.key === "Backspace" &&
            caret > 0 &&
            el.selectionStart === el.selectionEnd &&
            el.value[caret - 1] === " "
          ) {
            // Don't let Backspace merge two words; step back to the prior word.
            e.preventDefault()
            focusWord(wordIndexAt(el.value, caret) - 1)
          }
        }}
        onKeyUp={syncActive}
        onFocus={() => {
          setFocused(true)
          syncActive()
        }}
        onBlur={() => setFocused(false)}
        // pointer-events-none lets clicks fall through to the dot/container
        // handlers, which place the caret deterministically at a word boundary
        // (the input's own hidden text layout doesn't match the decorated view).
        className="pointer-events-none absolute inset-0 h-full w-full bg-transparent px-2.5 font-mono text-transparent caret-transparent outline-none"
      />

      {/* The shake and the red are invisible to a screen reader; say it out loud. */}
      <span role="status" aria-live="polite" className="sr-only">
        {rejectedIndex !== null ? `Word ${rejectedIndex + 1} is not in the word list` : ""}
      </span>
      </div>
    </motion.div>
  )
}

const indexOfFirstEmpty = (words: string[]) => {
  const i = words.findIndex((w) => !w)
  return i === -1 ? words.length - 1 : i
}
