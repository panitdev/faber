"use client"

import * as React from "react"
import { ChevronDown, RotateCcw } from "lucide-react"
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
  useTransform,
} from "framer-motion"
import { Popover as PopoverPrimitive } from "radix-ui"

import type { ThinkingSelection } from "@/lib/api"
import { Button } from "@charlotte-ui/button"
import { cn, DEFAULT_EASE } from "@charlotte-ui/utils"

/** One stop on the slider. */
export type ThinkingEffortLevel = {
  /** Handed back by `onChange` when this stop is chosen. */
  value: ThinkingSelection
  /** The level's name: the header, the reset's title, and assistive tech. */
  label: string
  /** Optional header rendering; `label` still names the level to AT. */
  display?: React.ReactNode
}

/**
 * Geometry in CSS px, mirroring the classes below. The thumb is larger than
 * the track, so it covers the filler's cut end and blooms past the track on
 * press. The thumb's centre travels from `THUMB_RADIUS` in from either end;
 * the filler and every level mark use that same travel.
 */
const TRACK_HEIGHT = 28
const THUMB_SIZE = 36
const THUMB_RADIUS = THUMB_SIZE / 2

/** Snap spring — firm, barely any overshoot (matches the Switch). */
const SETTLE = { type: "spring" as const, stiffness: 520, damping: 34, mass: 0.9 }
const INSTANT = { duration: 0 }
/** Lens grow/shrink — looser, so it wobbles a little like a droplet. */
const LENS = { type: "spring" as const, stiffness: 420, damping: 22, mass: 0.7 }
/** Glass ↔ solid cross-fade, timed apart from the lens spring (as the Switch). */
const MIX_IN = { duration: 0.12, ease: "easeOut" as const }
const MIX_OUT = { duration: 0.22, ease: "easeOut" as const }
const LENS_SCALE = 1.45
const GLASS_FADE_OUT_MS = 260
/** How long the lens stays up when the value changes from outside a press. */
const EXTERNAL_LENS_MS = 240
/** Header label swap: rolls up for a higher level, down for a lower one. */
const LABEL_SHIFT_PX = 8
const LABEL_SWAP = { duration: 0.2, ease: DEFAULT_EASE }
const LABEL_VARIANTS = {
  enter: (dir: number) => ({ y: dir * LABEL_SHIFT_PX, opacity: 0, filter: "blur(2px)" }),
  center: { y: 0, opacity: 1, filter: "blur(0px)" },
  exit: (dir: number) => ({ y: -dir * LABEL_SHIFT_PX, opacity: 0, filter: "blur(2px)" }),
}

/** The same clear lens the Switch and Button bloom on press. */
const GLASS_STYLE: React.CSSProperties = {
  background:
    "radial-gradient(120% 90% at 30% 15%, rgba(255,255,255,0.45), rgba(255,255,255,0.06) 55%, rgba(255,255,255,0.12))",
  backdropFilter: "blur(1.5px) saturate(1.8) brightness(1.08)",
  WebkitBackdropFilter: "blur(1.5px) saturate(1.8) brightness(1.08)",
  boxShadow: [
    "inset 0 0 0 0.75px rgba(255,255,255,0.7)",
    "inset 1.5px 2px 0.75px -0.75px rgba(255,255,255,0.85)",
    "inset -1.5px -2px 0.75px -0.75px rgba(255,255,255,0.45)",
    "inset 0 -4px 7px -4px rgba(0,0,0,0.12)",
    "0 4px 14px rgba(0,0,0,0.16)",
    "0 0.75px 3px rgba(0,0,0,0.08)",
  ].join(", "),
}

export type ThinkingEffortProps = {
  /**
   * The stops to offer, in display order. The caller owns both the set and
   * each stop's label — nothing here assumes a particular provider's ladder.
   */
  levels: ThinkingEffortLevel[]
  /** The session's pick, or `null` for the model's own default. */
  value: ThinkingSelection | null
  onChange: (selection: ThinkingSelection | null) => void
  /** The level `null` parks the thumb on. Defaults to the first stop. */
  defaultValue?: ThinkingSelection | null
  /** What `null` reads as in the header. Defaults to "Default". */
  defaultLabel?: string
  /** Header context, usually the model alias. */
  title?: string
  disabled?: boolean
  /** Which way the panel opens. `top` suits a composer pinned to the bottom. */
  side?: "top" | "bottom" | "left" | "right"
  align?: "start" | "center" | "end"
  className?: string
}

/**
 * How hard the model should think, as a slider over the levels the caller
 * declares — the expanded form of a thinking submenu.
 *
 * The thumb is parked at the level in effect. While the value is `null` that
 * is `defaultValue` (or the first stop), and the header reads `defaultLabel`
 * rather than claiming a pick nobody made; reset hands the decision back to
 * the model. Dragging commits the nearest stop as the pointer crosses it, and
 * the thumb and filler spring there together, so the control stays smooth
 * while snapping to real values only.
 *
 * Interaction follows the Button and Switch: pressing anywhere on the track
 * blooms the thumb into a clear glass lens, and a change made from outside —
 * reset, or an external value — flashes the same lens as it settles.
 * Visually it is the Switch's active state at slider scale: `bg-input` track,
 * `bg-primary` filler, `background`/`primary-foreground` thumb.
 */
export function ThinkingEffort(props: ThinkingEffortProps) {
  // A model without levels gets nothing — not a disabled control implying one.
  if (props.levels.length === 0) return null
  return <ThinkingEffortControl {...props} />
}

function ThinkingEffortControl({
  levels,
  value,
  onChange,
  defaultValue = null,
  defaultLabel = "Default",
  title,
  disabled = false,
  side = "top",
  align = "end",
  className,
}: ThinkingEffortProps) {
  const reduceMotion = useReducedMotion() ?? false
  const steps = levels.length - 1

  const defaultIndex = defaultValue === null
    ? 0
    : Math.max(0, levels.findIndex((level) => level.value === defaultValue))
  const index = value === null
    ? defaultIndex
    : Math.max(0, levels.findIndex((level) => level.value === value))
  const level = levels[index]
  const label = value === null ? defaultLabel : level.label
  const displayed = value === null ? defaultLabel : (level.display ?? level.label)

  // Which way the header label rolls: from the level last shown, so a reset
  // reads as moving toward wherever the default sits.
  const labelKey = value === null ? "\0default" : String(value)
  const [shownLabel, setShownLabel] = React.useState({ key: labelKey, index, dir: 1 })
  if (shownLabel.key !== labelKey) {
    setShownLabel({ key: labelKey, index, dir: index >= shownLabel.index ? 1 : -1 })
  }

  const rootRef = React.useRef<HTMLDivElement>(null)
  const indexRef = React.useRef(index)
  indexRef.current = index

  /* ---------- thumb + filler position, in level units ---------- */
  const pos = useMotionValue(index)
  // The thumb and the filler's cut end both ride a "rail": a box spanning
  // only the thumb's travel, half a thumb in from either end. Translating the
  // rail by a percentage of its own width moves it along exactly that travel,
  // so the motion is a transform: no layout per frame, no measuring.
  const railX = useTransform(pos, (p) => {
    const fraction = steps === 0 ? 0 : Math.min(Math.max(p / steps, 0), 1)
    return `${fraction * 100}%`
  })

  /* ---------- glass lens ---------- */
  const lens = useMotionValue(1)
  const glassScale = useTransform(lens, (v) => v / LENS_SCALE)
  // 0 = solid knob, 1 = glass. The fade runs on its own short clock: tied to
  // the lens size, it landed in the slow tail of the spring and dragged.
  // Both layers scale with the lens, so a fade at any size reads as a shrink.
  const mix = useMotionValue(0)
  const solidOpacity = useTransform(mix, (m) => 1 - m)
  // `glass` keeps the backdrop-filter layer mounted long enough to fade out.
  const [glass, setGlass] = React.useState(false)
  const lensUp = React.useRef(false)
  const lensTimer = React.useRef<ReturnType<typeof setTimeout>>(undefined)
  const glassTimer = React.useRef<ReturnType<typeof setTimeout>>(undefined)

  const animateLens = React.useCallback(
    (on: boolean) => {
      animate(lens, on ? LENS_SCALE : 1, LENS)
      animate(mix, on ? 1 : 0, on ? MIX_IN : MIX_OUT)
    },
    [lens, mix],
  )

  const showLens = React.useCallback(() => {
    if (reduceMotion) return
    clearTimeout(lensTimer.current)
    clearTimeout(glassTimer.current)
    lensUp.current = true
    setGlass(true)
    animateLens(true)
  }, [reduceMotion, animateLens])

  const hideLens = React.useCallback(
    (delay: number) => {
      clearTimeout(lensTimer.current)
      lensTimer.current = setTimeout(() => {
        lensUp.current = false
        animateLens(false)
        glassTimer.current = setTimeout(() => setGlass(false), GLASS_FADE_OUT_MS)
      }, delay)
    },
    [animateLens],
  )

  React.useEffect(
    () => () => {
      clearTimeout(lensTimer.current)
      clearTimeout(glassTimer.current)
    },
    [],
  )

  // Follow the committed level, and flash the lens when the change came from
  // outside a press (reset, a parent write) rather than from our own drag.
  const prevIndex = React.useRef(index)
  React.useEffect(() => {
    animate(pos, index, reduceMotion ? INSTANT : SETTLE)
    if (prevIndex.current !== index && !lensUp.current) {
      showLens()
      hideLens(EXTERNAL_LENS_MS)
    }
    prevIndex.current = index
  }, [index, pos, reduceMotion, showLens, hideLens])

  /* ---------- pointer ---------- */
  const dragging = React.useRef(false)

  const nearestIndex = React.useCallback(
    (clientX: number) => {
      const box = rootRef.current?.getBoundingClientRect()
      if (!box) return indexRef.current
      const usable = box.width - THUMB_SIZE
      if (usable <= 0 || steps === 0) return 0
      const fraction = (clientX - box.left - THUMB_RADIUS) / usable
      return Math.min(steps, Math.max(0, Math.round(fraction * steps)))
    },
    [steps],
  )

  const commit = React.useCallback(
    (next: number) => {
      if (next === indexRef.current) return
      const picked = levels[next]
      if (!picked) return
      indexRef.current = next
      onChange(picked.value)
    },
    [levels, onChange],
  )

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (disabled || event.button !== 0) return
    event.preventDefault()
    event.currentTarget.focus({ preventScroll: true })
    event.currentTarget.setPointerCapture(event.pointerId)
    dragging.current = true
    showLens()
    commit(nearestIndex(event.clientX))
  }

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return
    commit(nearestIndex(event.clientX))
  }

  const endDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return
    dragging.current = false
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId)
    }
    hideLens(90)
  }

  const stepTo = (next: number) => {
    commit(Math.min(steps, Math.max(0, next)))
  }

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (disabled) return
    switch (event.key) {
      case "ArrowRight":
      case "ArrowUp":
        stepTo(index + 1)
        break
      case "ArrowLeft":
      case "ArrowDown":
        stepTo(index - 1)
        break
      case "Home":
        stepTo(0)
        break
      case "End":
        stepTo(steps)
        break
      default:
        return
    }
    event.preventDefault()
  }

  return (
    <PopoverPrimitive.Root>
      <PopoverPrimitive.Trigger asChild>
        <Button
          variant="outline"
          disabled={disabled}
          aria-label={`Thinking effort: ${label}`}
          className={cn("group", className)}
        >
          <span>Thinking effort</span>
          <ChevronDown className="opacity-60 transition-transform duration-200 group-data-[state=open]:rotate-180 motion-reduce:transition-none" />
        </Button>
      </PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          side={side}
          align={align}
          sideOffset={8}
          collisionPadding={12}
          className={cn(
            "z-50 w-72 origin-(--radix-popover-content-transform-origin) rounded-2xl border border-border bg-popover p-4 text-popover-foreground shadow-lg outline-none",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
            "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95",
            "motion-reduce:animate-none",
          )}
        >
          <div className="flex items-center justify-between gap-3 pb-3">
            <div className="flex min-w-0 items-baseline gap-1.5">
              <span className="truncate text-base font-medium text-muted-foreground">
                {title ?? "Reasoning effort"}
              </span>
              {/* Outgoing and incoming labels share one grid cell, so they
                  cross over in place without either taking layout space. */}
              <span className="inline-grid min-w-0 text-base font-semibold text-primary">
                <AnimatePresence initial={false} custom={shownLabel.dir}>
                  <motion.span
                    key={labelKey}
                    custom={shownLabel.dir}
                    variants={LABEL_VARIANTS}
                    initial={reduceMotion ? false : "enter"}
                    animate="center"
                    exit={reduceMotion ? undefined : "exit"}
                    transition={LABEL_SWAP}
                    className="col-start-1 row-start-1 truncate"
                  >
                    {displayed}
                  </motion.span>
                </AnimatePresence>
              </span>
            </div>
            <Button
              variant="ghost"
              size="icon-xs"
              className="-mr-2 shrink-0 text-muted-foreground"
              aria-label="Use the model's default effort"
              title="Use the model's default"
              disabled={disabled || value === null}
              onClick={() => onChange(null)}
            >
              <RotateCcw />
            </Button>
          </div>

          <div
            ref={rootRef}
            role="slider"
            tabIndex={disabled ? undefined : 0}
            aria-label="Thinking effort"
            aria-orientation="horizontal"
            aria-valuemin={0}
            aria-valuemax={steps}
            aria-valuenow={index}
            aria-valuetext={label}
            aria-disabled={disabled || undefined}
            data-disabled={disabled || undefined}
            onPointerDown={handlePointerDown}
            onPointerMove={handlePointerMove}
            onPointerUp={endDrag}
            onPointerCancel={endDrag}
            onKeyDown={handleKeyDown}
            className="group/slider relative flex h-9 w-full touch-none items-center outline-none select-none data-[disabled]:pointer-events-none data-[disabled]:opacity-50"
          >
            <div
              aria-hidden
              className={cn(
                "relative w-full overflow-hidden rounded-full bg-input",
                "shadow-[inset_0_1px_2px_oklch(0_0_0_/_0.08)]",
              )}
              style={{ height: TRACK_HEIGHT }}
            >
              <motion.span
                className="absolute inset-y-0"
                style={{ left: THUMB_RADIUS, right: THUMB_RADIUS, x: railX }}
              >
                {/* Ends on the rail's left edge (the thumb's centre) and runs
                    far enough left to always reach the track's start; the
                    track's rounded overflow trims the rest. */}
                <span
                  className="absolute inset-y-0 right-full rounded-full bg-primary"
                  style={{ width: `calc(100% + ${THUMB_SIZE}px)` }}
                />
              </motion.span>
            </div>

            {/* Level marks, sitting under the thumb. They use the same travel
                as the thumb's centre, so a mark and the thumb align on a stop. */}
            <span aria-hidden className="pointer-events-none absolute inset-0">
              {levels.map((option, at) => (
                <span
                  key={option.value}
                  className={cn(
                    "absolute top-1/2 size-1 -translate-x-1/2 -translate-y-1/2 rounded-full transition-colors duration-200 motion-reduce:transition-none",
                    at < index
                      ? "bg-primary-foreground/40"
                      : "bg-muted-foreground/40",
                  )}
                  style={{
                    left: `calc(${THUMB_RADIUS}px + ${
                      steps === 0 ? 0 : at / steps
                    } * (100% - ${THUMB_SIZE}px))`,
                  }}
                />
              ))}
            </span>

            {/* The handle is larger than the track, so it sits over the
                filler's cut end instead of letting it show past the rim. */}
            <motion.div
              aria-hidden
              className="pointer-events-none absolute inset-y-0 z-10"
              style={{ left: THUMB_RADIUS, right: THUMB_RADIUS, x: railX }}
            >
              <div className="absolute top-1/2 left-0 size-9 -translate-x-1/2 -translate-y-1/2 rounded-full group-focus-visible/slider:ring-[3px] group-focus-visible/slider:ring-ring/50">
                {/* The solid knob rides the lens scale too, so glass and knob are
                    the same size on every frame and the cross-fade reads as one
                    body changing material, not a disc appearing inside a ring. */}
                <motion.span
                  className="absolute inset-0 rounded-full bg-background shadow-[0_1px_2px_oklch(0_0_0_/_0.2)] dark:bg-primary-foreground"
                  style={{ opacity: solidOpacity, scale: lens }}
                />
                {glass ? (
                  <motion.span
                    className="absolute top-1/2 left-1/2"
                    style={{
                      width: THUMB_SIZE * LENS_SCALE,
                      height: THUMB_SIZE * LENS_SCALE,
                      x: "-50%",
                      y: "-50%",
                      scale: glassScale,
                    }}
                  >
                    {/* Opacity goes on the blurred element itself, never on an
                        ancestor: an ancestor below opacity 1 becomes the backdrop
                        root, so the blur sees nothing behind it until the fade
                        ends and then pops in on the last frame. */}
                    <motion.span
                      className="absolute inset-0 rounded-full"
                      style={{ ...GLASS_STYLE, opacity: mix }}
                    />
                  </motion.span>
                ) : null}
              </div>
            </motion.div>
          </div>
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  )
}
