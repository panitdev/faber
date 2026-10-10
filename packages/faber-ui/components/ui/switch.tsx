import * as React from "react"
import {
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
  useSpring,
  useTransform,
  useVelocity,
} from "framer-motion"
import { Switch as SwitchPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"

/**
 * Geometry in CSS px. Track and thumb are drawn in one SVG, so the inset is
 * rendered at its exact position (antialiased) instead of being rounded to
 * device pixels per box — the gap stays even at any zoom / devicePixelRatio
 * and wherever the switch lands on the page. Fractional insets are fine.
 */
const GEOMETRY = {
  default: { width: 46, height: 20, inset: 2, thumbWidth: 25, shadowY: 0.75, shadowBlur: 1.25 },
  sm: { width: 36, height: 16, inset: 1.5, thumbWidth: 20, shadowY: 0.5, shadowBlur: 1 },
} as const

/** Position spring — firm, barely any overshoot. */
const SETTLE = { type: "spring", stiffness: 520, damping: 34, mass: 0.9 } as const
/** Lens grow/shrink — looser, so it wobbles a little like a droplet. */
const LENS = { type: "spring", stiffness: 420, damping: 22, mass: 0.7 } as const
const INSTANT = { duration: 0 } as const
const LENS_SCALE = 1.45
const GLASS_FADE_OUT_MS = 260
/** How long the lens stays up when `checked` changes from outside the switch. */
const EXTERNAL_LENS_MS = 240

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

/** Both icons sit in the same grid cell and cross-fade off the root's data-state. */
const iconLayer =
  "flex aspect-square h-[62%] items-center justify-center transition-all duration-300 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none [&_svg]:size-full [&_svg]:stroke-[3]"

/**
 * Drop-in replacement for the shadcn `Switch` with a Liquid Glass thumb:
 * same props (everything on Radix's `Switch.Root`, plus `size`, `checkedIcon`,
 * `uncheckedIcon`) and the same `data-slot`/`data-size` hooks.
 *
 * - Press: the pill thumb blooms into a clear glass lens.
 * - Drag/fling: the lens follows the pointer, stretches with velocity, and
 *   rubber-bands past the ends; release springs it to the nearest side.
 * - Tap/keyboard: the lens stays up briefly while it slides, then condenses.
 *
 * Idle cost is one small SVG: no layout reads, and the backdrop-filter layer
 * is mounted only while the lens is up, so long toggle lists stay cheap.
 * Sizing comes from `size` (see GEOMETRY). RTL is supported.
 * Honours `prefers-reduced-motion`: no lens, no stretch, instant moves.
 */
function Switch({
  className,
  style,
  size = "default",
  checkedIcon,
  uncheckedIcon,
  checked: checkedProp,
  defaultChecked,
  onCheckedChange,
  disabled,
  onClick,
  onPointerDown,
  onPointerMove,
  onPointerUp,
  onPointerCancel,
  ...props
}: React.ComponentProps<typeof SwitchPrimitive.Root> & {
  size?: "sm" | "default"
  /** Icon shown in the thumb while checked. */
  checkedIcon?: React.ReactNode
  /** Icon shown in the thumb while unchecked. */
  uncheckedIcon?: React.ReactNode
}) {
  const reduceMotion = useReducedMotion() ?? false
  const settle = reduceMotion ? INSTANT : SETTLE

  /* ---------- geometry ---------- */
  const g = GEOMETRY[size]
  const thumbHeight = g.height - g.inset * 2
  const start = g.inset
  const end = g.width - g.inset - g.thumbWidth
  const shadowId = "switch-shadow-" + React.useId().replace(/[^a-zA-Z0-9_-]/g, "")

  /* ---------- controlled / uncontrolled state (Radix stays controlled) ---------- */
  const [uncontrolled, setUncontrolled] = React.useState(defaultChecked ?? false)
  const isControlled = checkedProp !== undefined
  const checked = isControlled ? checkedProp : uncontrolled
  const setChecked = (next: boolean) => {
    if (!isControlled) setUncontrolled(next)
    onCheckedChange?.(next)
  }

  /* ---------- motion values ---------- */
  // Thumb's leading edge in px from the track's start edge.
  const pos = useMotionValue(checked ? end : start)
  const fill = useTransform(pos, [start, end], [0, 1], { clamp: true })

  const velocity = useVelocity(pos)
  const rawStretch = useTransform(velocity, [-240, 0, 240], [1.2, 1, 1.2], { clamp: true })
  const scaleX = useSpring(rawStretch, { stiffness: 600, damping: 30 })
  const scaleY = useTransform(scaleX, (s) => 1 / Math.sqrt(s))

  const lens = useMotionValue(1) // bloom scale
  // The glass is laid out at its full bloomed size and scaled *down* at rest:
  // it's a composited (backdrop-filter) layer, so scaling it up would stretch a
  // small bitmap and pixelate the rim highlight.
  const glassScale = useTransform(lens, (v) => v / LENS_SCALE)
  const mix = useMotionValue(0) // 0 = solid pill, 1 = glass
  const solidOpacity = useTransform(mix, (m) => 1 - m)

  const drag = React.useRef<{ startX: number; origin: number; dir: number; moved: boolean } | null>(null)
  const suppressClick = React.useRef(false)

  /* ---------- lens ---------- */
  // `glass` keeps the backdrop-filter layer mounted just long enough to fade out.
  const [lensed, setLensed] = React.useState(false)
  const [glass, setGlass] = React.useState(false)
  const lensUp = React.useRef(false) // mirrors `lensed` without waiting for a render
  const lensTimer = React.useRef<ReturnType<typeof setTimeout>>(undefined)
  const glassTimer = React.useRef<ReturnType<typeof setTimeout>>(undefined)
  const animateLens = (on: boolean) => {
    animate(lens, on ? LENS_SCALE : 1, LENS)
    animate(mix, on ? 1 : 0, { duration: on ? 0.12 : 0.22, ease: "easeOut" })
  }
  const showLens = () => {
    if (reduceMotion) return
    clearTimeout(lensTimer.current)
    clearTimeout(glassTimer.current)
    lensUp.current = true
    setGlass(true)
    setLensed(true)
    animateLens(true)
  }
  const hideLens = (delay: number) => {
    clearTimeout(lensTimer.current)
    lensTimer.current = setTimeout(() => {
      lensUp.current = false
      setLensed(false)
      animateLens(false)
      glassTimer.current = setTimeout(() => setGlass(false), GLASS_FADE_OUT_MS)
    }, delay)
  }
  React.useEffect(
    () => () => {
      clearTimeout(lensTimer.current)
      clearTimeout(glassTimer.current)
    },
    []
  )

  // Follow `checked` (taps, keyboard, controlled updates) and size changes.
  // A change from outside (controlled `checked`, form reset…) blooms the lens
  // while it slides, like a tap; presses, drags and keys already have it up.
  const prevChecked = React.useRef(checked)
  React.useEffect(() => {
    if (drag.current) return
    animate(pos, checked ? end : start, settle)
    if (prevChecked.current !== checked && !lensUp.current) {
      showLens()
      hideLens(EXTERNAL_LENS_MS)
    }
    prevChecked.current = checked
  }, [checked, start, end]) // eslint-disable-line react-hooks/exhaustive-deps

  const rubberBand = (v: number) =>
    v < start ? start + (v - start) * 0.25 : v > end ? end + (v - end) * 0.25 : v

  const hasIcons = Boolean(checkedIcon || uncheckedIcon)

  return (
    <SwitchPrimitive.Root
      {...props}
      data-slot="switch"
      data-size={size}
      data-lensed={lensed || undefined}
      checked={checked}
      onCheckedChange={setChecked}
      disabled={disabled}
      style={{ width: g.width, height: g.height, ...style }}
      className={cn(
        "peer group/switch relative inline-block shrink-0 touch-none rounded-full p-0 outline-none select-none focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      onPointerDown={(e) => {
        onPointerDown?.(e)
        if (e.defaultPrevented || disabled || e.button !== 0) return
        e.currentTarget.setPointerCapture(e.pointerId)
        pos.stop()
        suppressClick.current = false
        drag.current = {
          startX: e.clientX,
          origin: pos.get(),
          dir: getComputedStyle(e.currentTarget).direction === "rtl" ? -1 : 1,
          moved: false,
        }
        showLens()
      }}
      onPointerMove={(e) => {
        onPointerMove?.(e)
        const d = drag.current
        if (!d) return
        const dx = (e.clientX - d.startX) * d.dir
        if (!d.moved && Math.abs(dx) < 3) return
        d.moved = true
        pos.set(rubberBand(d.origin + dx))
      }}
      onPointerUp={(e) => {
        onPointerUp?.(e)
        const d = drag.current
        if (!d) return
        drag.current = null
        if (!d.moved) {
          hideLens(200) // tap: Radix toggles on the click that follows
          return
        }
        // Drag: we decide the state here and swallow the click that follows.
        suppressClick.current = true
        setTimeout(() => (suppressClick.current = false), 0)
        const next = pos.get() > (start + end) / 2
        if (next !== checked) setChecked(next) // effect springs it home
        else animate(pos, checked ? end : start, settle)
        hideLens(90)
      }}
      onPointerCancel={(e) => {
        onPointerCancel?.(e)
        if (!drag.current) return
        drag.current = null
        animate(pos, checked ? end : start, settle)
        hideLens(0)
      }}
      onClick={(e) => {
        onClick?.(e)
        if (suppressClick.current) {
          suppressClick.current = false
          e.preventDefault() // stops Radix's own toggle
          return
        }
        // Keyboard (Space/Enter) clicks have detail 0: flash the lens like a tap.
        if (e.detail === 0 && !disabled && !e.defaultPrevented) {
          showLens()
          hideLens(220)
        }
      }}
    >
      {/* Track + solid thumb share one coordinate system → even gap at any scale. */}
      <svg
        aria-hidden
        width={g.width}
        height={g.height}
        viewBox={`0 0 ${g.width} ${g.height}`}
        className="pointer-events-none absolute inset-0 overflow-visible rtl:-scale-x-100"
      >
        <defs>
          <filter id={shadowId} x="-50%" y="-50%" width="200%" height="200%">
            <feDropShadow dx="0" dy={g.shadowY} stdDeviation={g.shadowBlur} floodColor="#000" floodOpacity="0.2" />
          </filter>
        </defs>
        <rect width={g.width} height={g.height} rx={g.height / 2} className="fill-input dark:fill-input/80" />
        <motion.rect
          width={g.width}
          height={g.height}
          rx={g.height / 2}
          className="fill-primary"
          style={{ opacity: fill }}
        />
        <motion.g style={{ x: pos, scale: lens, opacity: solidOpacity }}>
          <motion.g style={reduceMotion ? undefined : { scaleX, scaleY }}>
            <rect
              y={g.inset}
              width={g.thumbWidth}
              height={thumbHeight}
              rx={thumbHeight / 2}
              filter={`url(#${shadowId})`}
              className="fill-background dark:group-data-[state=checked]/switch:fill-primary-foreground dark:group-data-[state=unchecked]/switch:fill-foreground"
            />
          </motion.g>
        </motion.g>
      </svg>

      {/* Glass lens (needs backdrop-filter, so HTML) and icons ride on top of the thumb. */}
      {glass || hasIcons ? (
        <span aria-hidden className="pointer-events-none absolute inset-0 rtl:-scale-x-100">
          <motion.span
            data-slot="switch-thumb"
            className="absolute top-0 left-0"
            style={{ width: g.thumbWidth, height: thumbHeight, x: pos, y: g.inset }}
          >
            {glass ? (
              <motion.span
                className="absolute top-1/2 left-1/2"
                style={{
                  width: g.thumbWidth * LENS_SCALE,
                  height: thumbHeight * LENS_SCALE,
                  x: "-50%",
                  y: "-50%",
                  scale: glassScale,
                  opacity: mix,
                }}
              >
                <motion.span
                  className="absolute inset-0 rounded-full"
                  style={reduceMotion ? GLASS_STYLE : { ...GLASS_STYLE, scaleX, scaleY }}
                />
              </motion.span>
            ) : null}
            {hasIcons ? (
              // The cell is pinned to the thumb (minmax(0,1fr)) so the icons' 62% height
              // resolves against the thumb, not against the SVG's intrinsic 24px.
              <span className="absolute inset-0 grid grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] place-items-center transition-opacity duration-150 group-data-[lensed]/switch:opacity-0 rtl:-scale-x-100 [&>*]:col-start-1 [&>*]:row-start-1">
                <span className={cn(iconLayer, "rotate-90 scale-50 text-muted-foreground opacity-0 group-data-[state=unchecked]/switch:rotate-0 group-data-[state=unchecked]/switch:scale-100 group-data-[state=unchecked]/switch:opacity-100")}>
                  {uncheckedIcon}
                </span>
                <span className={cn(iconLayer, "-rotate-90 scale-50 text-primary opacity-0 group-data-[state=checked]/switch:rotate-0 group-data-[state=checked]/switch:scale-100 group-data-[state=checked]/switch:opacity-100")}>
                  {checkedIcon}
                </span>
              </span>
            ) : null}
          </motion.span>
        </span>
      ) : null}
    </SwitchPrimitive.Root>
  )
}

export { Switch }
