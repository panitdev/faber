"use client"

import * as React from "react"
import { Loader2 } from "lucide-react"
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
  useSpring,
  useTransform,
  useVelocity,
} from "framer-motion"
import { Slot } from "radix-ui"
import { cva, type VariantProps } from "class-variance-authority"

import { cn } from "@/lib/utils"

const buttonVariants = cva(
  "relative inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-full border text-sm font-medium select-none outline-none transition-colors disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0 focus-visible:ring-[3px] focus-visible:ring-ring/50 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40",
  {
    variants: {
      variant: {
        default:
          "border-none bg-primary text-primary-foreground shadow-[0_1px_0_0_oklch(1_0_0_/_0.15)_inset,0_1px_2px_oklch(0_0_0_/_0.10)] hover:bg-primary/90",
        destructive:
          "border-none bg-destructive text-white shadow-[0_1px_0_0_oklch(1_0_0_/_0.12)_inset,0_1px_2px_oklch(0_0_0_/_0.10)] hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:focus-visible:ring-destructive/40",
        outline:
          "border-border bg-card text-foreground shadow-[0_1px_2px_oklch(0_0_0_/_0.04)] hover:border-foreground/30",
        secondary:
          "border-none bg-secondary text-secondary-foreground hover:bg-secondary/85",
        ghost:
          "border-none bg-transparent text-foreground hover:bg-muted",
        link:
          "h-auto rounded-none border-transparent bg-transparent px-0 py-0 text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-10 px-5 py-2 has-[>svg]:px-4",
        xs: "h-8 px-3 py-1.5 text-xs has-[>svg]:px-2.5",
        sm: "h-9 px-4 py-1.5 has-[>svg]:px-3.5",
        lg: "h-11 px-6 py-2.5 has-[>svg]:px-5",
        icon: "size-10",
        "icon-xs": "size-8",
        "icon-sm": "size-9",
        "icon-lg": "size-11",
      },
      fullWidth: {
        true: "w-full",
      },
    },
    compoundVariants: [
      {
        variant: "link",
        className: "min-h-0 px-0 py-0",
      },
    ],
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
)

/** Lens bloom — loose, so it wobbles a little like a droplet (matches the switch lens). */
const BLOOM = { type: "spring", stiffness: 420, damping: 22, mass: 0.7 } as const
/** Lens return after a drag — firm, barely any overshoot. */
const SETTLE = { type: "spring", stiffness: 520, damping: 34, mass: 0.9 } as const
const STRETCH = { stiffness: 600, damping: 30 } as const
/** The lens is the button's own pill, grown by this many px on every side. */
const BLOOM_PX = 5
/** How far the lens leans towards a dragging pointer. */
const MAX_PULL = 6
const LENS_FADE_OUT_MS = 280

const GLASS_STYLE: React.CSSProperties = {
  background:
    "radial-gradient(120% 90% at 30% 15%, rgba(255,255,255,0.45), rgba(255,255,255,0.06) 55%, rgba(255,255,255,0.12))",
  backdropFilter: "blur(2px) saturate(1.8) brightness(1.08)",
  WebkitBackdropFilter: "blur(2px) saturate(1.8) brightness(1.08)",
  boxShadow: [
    "inset 0 0 0 0.5px rgba(255,255,255,0.7)",
    "inset 1px 1.5px 0.5px -0.5px rgba(255,255,255,0.85)",
    "inset -1px -1.5px 0.5px -0.5px rgba(255,255,255,0.45)",
    "inset 0 -3px 5px -3px rgba(0,0,0,0.12)",
    "0 3px 10px rgba(0,0,0,0.16)",
    "0 0.5px 2px rgba(0,0,0,0.08)",
  ].join(", "),
}

/** Soft limit: linear near 0, easing out towards ±MAX_PULL. */
const rubberBand = (d: number) => MAX_PULL * Math.tanh(d / (MAX_PULL * 5))

type ButtonProps = React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean
    loading?: boolean
    loadingText?: React.ReactNode
    animateWidth?: boolean
  }

/**
 * shadcn-compatible `Button` with the same Liquid Glass feel as `Switch`.
 * Layers, bottom to top: the solid button, a glass lens, the label.
 *
 * - Press: a copy of the button's pill blooms out past its edges as a clear
 *   glass lens over the solid fill; the label stays crisp on top.
 * - Hold and drag: the lens leans towards the pointer (rubber-banded to a few
 *   px) and stretches with velocity. Releasing outside still cancels the
 *   click, as with a native button.
 * - Release: the lens condenses back into the button and fades. Keyboard
 *   (Space/Enter) flashes it.
 *
 * The lens (and its backdrop-filter) is mounted only while it is visible.
 * `link` buttons and `asChild` skip it; `prefers-reduced-motion` turns it off.
 */
function Button({
  className,
  variant = "default",
  size = "default",
  asChild = false,
  loading = false,
  loadingText,
  animateWidth = false,
  disabled,
  children,
  fullWidth,
  onClick,
  onPointerDown,
  ...props
}: ButtonProps) {
  const isDisabled = disabled || loading
  const reduceMotion = useReducedMotion() ?? false
  const glassy = variant !== "link" && !reduceMotion

  /* ---------- Liquid Glass lens ---------- */
  // Lean towards the pointer, in px from rest.
  const pullX = useMotionValue(0)
  const pullY = useMotionValue(0)

  // Velocity stretch along each axis, volume-preserving like the switch thumb.
  const stretchX = useSpring(useTransform(useVelocity(pullX), [-200, 0, 200], [1.08, 1, 1.08], { clamp: true }), STRETCH)
  const stretchY = useSpring(useTransform(useVelocity(pullY), [-200, 0, 200], [1.12, 1, 1.12], { clamp: true }), STRETCH)
  const scaleX = useTransform(() => stretchX.get() / Math.sqrt(stretchY.get()))
  const scaleY = useTransform(() => stretchY.get() / Math.sqrt(stretchX.get()))

  // Growth in px past each edge — px rather than scale so the pill's ends stay round.
  const grow = useMotionValue(0)
  const inset = useTransform(grow, (g) => -g)
  const opacity = useMotionValue(0)

  const [lens, setLens] = React.useState(false)
  const hideTimer = React.useRef<ReturnType<typeof setTimeout>>(undefined)
  const flashTimer = React.useRef<ReturnType<typeof setTimeout>>(undefined)
  const press = React.useRef<{ cleanup: () => void } | null>(null)

  const showLens = () => {
    clearTimeout(hideTimer.current)
    clearTimeout(flashTimer.current)
    setLens(true)
    animate(grow, BLOOM_PX, BLOOM)
    animate(opacity, 1, { duration: 0.12, ease: "easeOut" })
  }
  const hideLens = () => {
    animate(grow, 0, BLOOM)
    animate(pullX, 0, SETTLE)
    animate(pullY, 0, SETTLE)
    animate(opacity, 0, { duration: 0.22, ease: "easeOut" })
    hideTimer.current = setTimeout(() => setLens(false), LENS_FADE_OUT_MS)
  }
  const endPress = () => {
    if (!press.current) return
    press.current.cleanup()
    press.current = null
    hideLens()
  }

  React.useEffect(
    () => () => {
      press.current?.cleanup()
      clearTimeout(hideTimer.current)
      clearTimeout(flashTimer.current)
    },
    []
  )
  // A press that gets disabled midway (e.g. it started loading) lets go.
  React.useEffect(() => {
    if (isDisabled) endPress()
  }, [isDisabled]) // eslint-disable-line react-hooks/exhaustive-deps

  const buttonRef = React.useRef<HTMLButtonElement>(null)
  const spacerRef = React.useRef<HTMLSpanElement>(null)
  const animatedWidth = useMotionValue(0)
  const hasMeasured = React.useRef(false)
  const widthAnimation = React.useRef<ReturnType<typeof animate> | null>(null)

  React.useLayoutEffect(() => {
    if (!animateWidth) {
      hasMeasured.current = false
      return
    }
    if (!buttonRef.current || !spacerRef.current) return
    const cs = getComputedStyle(buttonRef.current)
    const w = spacerRef.current.offsetWidth + parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight)
    if (!hasMeasured.current) {
      animatedWidth.jump(w)
      hasMeasured.current = true
    } else {
      widthAnimation.current?.stop()
      widthAnimation.current = animate(animatedWidth, w, { type: "spring", stiffness: 350, damping: 35 })
    }
  }, [loading, animateWidth, animatedWidth])

  const content = (
    <>
      {/* Invisible in-flow spacer so the button holds its natural width */}
      <span ref={spacerRef} aria-hidden className="invisible shrink-0 pointer-events-none inline-flex items-center gap-2">
        {loading ? (
          <>
            <Loader2 className="size-4" />
            {loadingText ?? children}
          </>
        ) : (
          children
        )}
      </span>
      <span className="absolute inset-0 overflow-hidden rounded-[inherit]">
        <AnimatePresence mode="popLayout" initial={false}>
          {loading ? (
            <motion.span
              key="loading"
              initial={{ y: "100%" }}
              animate={{ y: 0 }}
              exit={{ y: "100%" }}
              transition={{ type: "spring", stiffness: 350, damping: 18 }}
              className="absolute inset-0 inline-flex items-center justify-center gap-2"
            >
              <Loader2 className="size-4 animate-spin" />
              {loadingText ?? children}
            </motion.span>
          ) : (
            <motion.span
              key="idle"
              initial={{ y: "-100%" }}
              animate={{ y: 0 }}
              exit={{ y: "-100%" }}
              transition={{ type: "spring", stiffness: 350, damping: 18 }}
              className="absolute inset-0 inline-flex items-center justify-center gap-2"
            >
              {children}
            </motion.span>
          )}
        </AnimatePresence>
      </span>
    </>
  )

  if (asChild) {
    return (
      <Slot.Root
        data-slot="button"
        data-variant={variant}
        data-size={size}
        className={cn(buttonVariants({ variant, size, fullWidth, className }))}
        aria-disabled={isDisabled}
        onClick={(event) => {
          if (isDisabled) {
            event.preventDefault()
            event.stopPropagation()
            return
          }

          onClick?.(event as React.MouseEvent<HTMLButtonElement>)
        }}
        {...props}
      >
        {children}
      </Slot.Root>
    )
  }


  const { style: externalStyle, ...restProps } = props as React.ComponentProps<typeof motion.button>

  return (
    <motion.button
      ref={buttonRef}
      data-slot="button"
      data-variant={variant}
      data-size={size}
      whileHover={isDisabled ? undefined : { y: -1 }}
      transition={{ type: "spring", stiffness: 600, damping: 30 }}
      style={{ ...externalStyle, ...(animateWidth ? { width: animatedWidth } : {}) }}
      className={cn(buttonVariants({ variant, size, fullWidth, className }))}
      disabled={isDisabled}
      {...restProps}
      onPointerDown={(e) => {
        onPointerDown?.(e)
        if (e.defaultPrevented || isDisabled || !glassy || e.button !== 0) return
        press.current?.cleanup()
        const startX = e.clientX
        const startY = e.clientY
        // No pointer capture: releasing outside the button must still cancel the click.
        const move = (ev: PointerEvent) => {
          if (ev.pointerId !== e.pointerId) return
          pullX.set(rubberBand(ev.clientX - startX))
          pullY.set(rubberBand(ev.clientY - startY))
        }
        const up = (ev: PointerEvent) => {
          if (ev.pointerId === e.pointerId) endPress()
        }
        window.addEventListener("pointermove", move)
        window.addEventListener("pointerup", up)
        window.addEventListener("pointercancel", up)
        press.current = {
          cleanup: () => {
            window.removeEventListener("pointermove", move)
            window.removeEventListener("pointerup", up)
            window.removeEventListener("pointercancel", up)
          },
        }
        showLens()
      }}
      onClick={(e) => {
        onClick?.(e)
        // Keyboard (Space/Enter) clicks have detail 0: flash the lens like a tap.
        if (e.detail === 0 && glassy && !isDisabled) {
          showLens()
          flashTimer.current = setTimeout(hideLens, 180)
        }
      }}
    >
      {/* Glass lens: the button's pill, grown past its edges, between the fill and the label. */}
      {lens ? (
        <motion.span
          aria-hidden
          data-slot="button-glass"
          className="pointer-events-none absolute rounded-[inherit]"
          style={{ ...GLASS_STYLE, inset, x: pullX, y: pullY, scaleX, scaleY, opacity }}
        />
      ) : null}
      {content}
    </motion.button>
  )
}

export { Button, buttonVariants }
