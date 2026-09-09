"use client"

import * as React from "react"
import { AnimatePresence, motion, useReducedMotion } from "framer-motion"

import { FaberMark, MARK_ASPECT } from "@/components/thread/faber-mark"
import { PANIT_DEFAULT_EASE } from "@/lib/motion"
import { cn } from "@/lib/utils"

const WORKING_TEXTS = [
  "Working",
  "Figuring",
  "Shaping",
  "Refining",
  "Thinking",
  "Considering",
  "Forming",
  "Weaving",
]

const SWAP_INTERVAL_MS = 5000
const MARK_SIZE = 36

const SLIDE_TRANSITION = {
  duration: 0.32,
  ease: PANIT_DEFAULT_EASE,
} as const

const DIGIT_TRANSITION = {
  duration: 0.24,
  ease: PANIT_DEFAULT_EASE,
} as const

function pickText(exclude?: string): string {
  const pool = exclude ? WORKING_TEXTS.filter((text) => text !== exclude) : WORKING_TEXTS
  return pool[Math.floor(Math.random() * pool.length)]
}

// ---------------------------------------------------------------------------
// Elapsed-time counter
// ---------------------------------------------------------------------------

function useElapsed(active: boolean) {
  const [elapsed, setElapsed] = React.useState(0)
  const originRef = React.useRef(0)

  React.useEffect(() => {
    if (!active) {
      setElapsed(0)
      return
    }
    originRef.current = Date.now()
    const tick = () => setElapsed(Math.floor((Date.now() - originRef.current) / 1000))
    tick()
    const id = setInterval(tick, 1000)
    return () => clearInterval(id)
  }, [active])

  return elapsed
}

function SlotDigit({ char, reduce }: { char: string; reduce: boolean }) {
  return (
    <span className="relative inline-flex h-5 items-center overflow-hidden tabular-nums" style={{ width: "0.58em" }}>
      <AnimatePresence mode="popLayout" initial={false}>
        <motion.span
          key={char}
          className="inline-flex w-full justify-center"
          initial={reduce ? false : { y: "100%" }}
          animate={{ y: 0 }}
          exit={{ y: "-100%" }}
          transition={reduce ? { duration: 0 } : DIGIT_TRANSITION}
        >
          {char}
        </motion.span>
      </AnimatePresence>
    </span>
  )
}

function ElapsedTime({ seconds, reduce }: { seconds: number; reduce: boolean }) {
  const hasMinutes = seconds >= 60
  const m = Math.floor(seconds / 60)
  const s = seconds % 60

  const minuteChars = hasMinutes ? String(m).split("") : []
  const secondChars = (hasMinutes ? String(s).padStart(2, "0") : String(s)).split("")

  const minuteRef = React.useRef<HTMLSpanElement>(null)
  const [minuteWidth, setMinuteWidth] = React.useState(0)

  React.useLayoutEffect(() => {
    if (minuteRef.current) {
      setMinuteWidth(hasMinutes ? minuteRef.current.offsetWidth : 0)
    }
  }, [hasMinutes, m])

  return (
    <span className="inline-flex items-center text-xs text-muted-foreground/70">
      {/* Hidden measurer for minute section */}
      {hasMinutes && (
        <span
          ref={minuteRef}
          className="pointer-events-none invisible absolute whitespace-nowrap tabular-nums text-xs"
          aria-hidden
        >
          {String(m)}m{" "}
        </span>
      )}

      {/* Minute section — width-animates in at the 60s boundary */}
      <motion.span
        className="inline-flex items-center overflow-hidden"
        initial={false}
        animate={{ width: minuteWidth }}
        transition={reduce ? { duration: 0 } : SLIDE_TRANSITION}
      >
        {hasMinutes && (
          <>
            {minuteChars.map((d, i) => {
              const pos = minuteChars.length - 1 - i
              return <SlotDigit key={`m${pos}`} char={d} reduce={reduce} />
            })}
            <span>m{" "}</span>
          </>
        )}
      </motion.span>

      {/* Seconds digits — keyed from the right so carries animate correctly */}
      {secondChars.map((d, i) => {
        const pos = secondChars.length - 1 - i
        return <SlotDigit key={`s${pos}`} char={d} reduce={reduce} />
      })}
      <span>s</span>
    </span>
  )
}

// ---------------------------------------------------------------------------
// Main indicator
// ---------------------------------------------------------------------------

export function FaberIndicator({
  className,
  nodeSize = 64,
  text: textProp,
  working = false,
}: {
  className?: string
  nodeSize?: number
  text?: string
  working?: boolean
}) {
  const reduce = useReducedMotion()
  const [internalText, setInternalText] = React.useState(pickText)
  const measureRef = React.useRef<HTMLSpanElement>(null)
  const [textWidth, setTextWidth] = React.useState(0)
  const seqRef = React.useRef(0)
  const prevTextRef = React.useRef<string | null>(null)

  const text = textProp ?? internalText
  const elapsed = useElapsed(working)

  if (prevTextRef.current !== text) {
    seqRef.current++
    prevTextRef.current = text
  }

  React.useEffect(() => {
    if (!working || textProp != null) return

    const id = setInterval(() => setInternalText((prev) => pickText(prev)), SWAP_INTERVAL_MS)
    return () => clearInterval(id)
  }, [working, textProp])

  React.useLayoutEffect(() => {
    if (measureRef.current) {
      setTextWidth(measureRef.current.offsetWidth)
    }
  }, [text])

  return (
    <div
      className={cn("flex items-center gap-2", className)}
      style={{ marginLeft: nodeSize / 2 - (MARK_SIZE * MARK_ASPECT) / 2 }}
    >
      <FaberMark size={MARK_SIZE} working={working} />

      {working ? (
        <div className="flex items-center gap-2">
          <span
            ref={measureRef}
            className="pointer-events-none invisible absolute whitespace-nowrap text-sm"
            aria-hidden
          >
            {text}
          </span>

          <motion.div
            className="relative h-5"
            initial={false}
            animate={{ width: textWidth }}
            transition={reduce ? { duration: 0 } : SLIDE_TRANSITION}
          >
            <AnimatePresence mode="popLayout" initial={false}>
              <motion.span
                key={seqRef.current}
                className={cn(
                  "inline-block whitespace-nowrap text-sm",
                  reduce
                    ? "text-muted-foreground"
                    : "animate-text-shimmer bg-clip-text text-transparent [background-image:linear-gradient(100deg,var(--muted-foreground)_10%,var(--foreground)_50%,var(--muted-foreground)_90%)] [background-size:200%_100%]",
                )}
                initial={reduce ? false : { opacity: 0, y: "100%", filter: "blur(2px)" }}
                animate={{ opacity: 1, y: 0, filter: "blur(0px)" }}
                exit={{ opacity: 0, y: "-100%", filter: "blur(2px)" }}
                transition={reduce ? { duration: 0 } : SLIDE_TRANSITION}
              >
                {text}
              </motion.span>
            </AnimatePresence>
          </motion.div>

          <span className="relative inline-flex h-1.5 w-1.5 translate-y-px">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-foreground/60 opacity-75" />
            <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-foreground/60" />
          </span>

          <ElapsedTime seconds={elapsed} reduce={reduce ?? false} />
        </div>
      ) : null}
    </div>
  )
}
