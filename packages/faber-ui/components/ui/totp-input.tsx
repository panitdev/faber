"use client"

import * as React from "react"
import { AnimatePresence, motion } from "framer-motion"

import { cn, DEFAULT_EASE } from "@charlotte-ui/utils"

type TotpInputProps = {
  /** Controlled value — a string of up to `length` digits. */
  value: string
  onChange: (value: string) => void
  /** Number of digit slots. Defaults to 6. */
  length?: number
  /** Fired once the last slot is filled, with the full code. */
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

const onlyDigits = (s: string) => s.replace(/\D/g, "")

export function TotpInput({
  value,
  onChange,
  length = 6,
  onComplete,
  error = false,
  disabled = false,
  autoFocus = false,
  "aria-label": ariaLabel = "One-time passcode",
  id,
  className,
}: TotpInputProps) {
  const inputRef = React.useRef<HTMLInputElement | null>(null)
  const [focused, setFocused] = React.useState(false)
  const wasComplete = React.useRef(false)

  const code = onlyDigits(value).slice(0, length)
  const digits = Array.from({ length }, (_, i) => code[i] ?? "")
  const isComplete = code.length === length
  // The slot the next keystroke will fill (clamped to the last slot when full).
  const activeIndex = Math.min(code.length, length - 1)

  // Fire onComplete on the transition into a fully filled code.
  React.useEffect(() => {
    if (isComplete && !wasComplete.current) onComplete?.(code)
    wasComplete.current = isComplete
  }, [isComplete, code, onComplete])

  React.useEffect(() => {
    if (autoFocus) inputRef.current?.focus()
  }, [autoFocus])

  // The row is one big hitbox: any click focuses the field and parks the caret
  // at the end, so typing always appends and Backspace removes the last digit.
  const focusEnd = () => {
    const el = inputRef.current
    if (!el) return
    el.focus()
    const end = el.value.length
    el.setSelectionRange(end, end)
  }

  return (
    <motion.div
      role="group"
      aria-label={ariaLabel}
      animate={error ? { x: [0, -6, 6, -4, 4, 0] } : { x: 0 }}
      transition={
        error
          ? { duration: 0.4, ease: DEFAULT_EASE }
          : { duration: 0.2, ease: DEFAULT_EASE }
      }
      onMouseDown={(e) => {
        if (disabled) return
        // Take over the click ourselves so gaps and circles behave identically.
        e.preventDefault()
        focusEnd()
      }}
      className={cn(
        "relative flex w-fit items-center gap-2 py-1 sm:gap-2.5",
        !disabled && "cursor-text",
        className
      )}
    >
      {digits.map((digit, index) => {
        const isActive = focused && index === activeIndex
        const isActiveEmpty = isActive && !digit

        return (
          <motion.div
            key={index}
            animate={{
              // Filled slots settle inward; the active empty slot lifts.
              scale: digit ? 0.9 : isActive ? 1.04 : 1,
              y: isActiveEmpty ? -1 : 0,
            }}
            transition={{ type: "spring", stiffness: 480, damping: 28 }}
            className={cn(
              "relative flex h-12 w-12 items-center justify-center rounded-full border text-2xl font-semibold text-foreground transition-colors duration-200 sm:h-14 sm:w-14",
              error
                ? "border-destructive/70 bg-card"
                : digit
                  ? "border-transparent bg-muted"
                  : isActive
                    ? "border-primary bg-card ring-4 ring-primary/15"
                    : "border-border bg-card",
              disabled && "opacity-60"
            )}
          >
            <AnimatePresence mode="popLayout" initial={false}>
              {digit ? (
                <motion.span
                  key={digit + index}
                  initial={{ scale: 0.4, y: 6, opacity: 0 }}
                  animate={{ scale: 1, y: 0, opacity: 1 }}
                  exit={{ scale: 0.4, y: -6, opacity: 0 }}
                  transition={{ type: "spring", stiffness: 520, damping: 26 }}
                  className="tabular-nums leading-none"
                >
                  {digit}
                </motion.span>
              ) : null}
            </AnimatePresence>

            {isActiveEmpty && (
              <motion.span
                aria-hidden="true"
                className="absolute h-6 w-[2px] rounded-full bg-primary"
                animate={{ opacity: [1, 1, 0, 0] }}
                transition={{ duration: 1, repeat: Infinity, ease: "linear" }}
              />
            )}
          </motion.div>
        )
      })}

      {/* One transparent input spanning the whole row drives everything —
          typing, Backspace, and paste all flow through its native value. */}
      <input
        ref={inputRef}
        id={id}
        value={code}
        inputMode="numeric"
        autoComplete="one-time-code"
        pattern="[0-9]*"
        maxLength={length}
        disabled={disabled}
        aria-label={ariaLabel}
        onChange={(e) => onChange(onlyDigits(e.target.value).slice(0, length))}
        onFocus={(e) => {
          setFocused(true)
          const end = e.target.value.length
          e.target.setSelectionRange(end, end)
        }}
        onBlur={() => setFocused(false)}
        // Keep the caret pinned to the end even if a click lands mid-string.
        onSelect={(e) => {
          const el = e.currentTarget
          if (el.selectionStart !== el.value.length) focusEnd()
        }}
        className="absolute inset-0 h-full w-full cursor-text bg-transparent text-center text-transparent caret-transparent outline-none"
      />
    </motion.div>
  )
}
