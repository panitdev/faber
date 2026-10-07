import * as React from "react"
import { motion, useReducedMotion } from "framer-motion"
import { Switch as SwitchPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"

/** Both icons sit in the same grid cell and cross-fade off the root's data-state. */
const iconLayer =
  "flex size-[62%] items-center justify-center transition-all duration-300 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none [&_svg]:size-full [&_svg]:stroke-[3]"

/**
 * Drop-in replacement for the shadcn `Switch`: same props (everything on
 * Radix's `Switch.Root`, plus `size`), same `data-slot`/`data-size` hooks and
 * the same resting geometry, so existing call sites and styles keep working.
 *
 * The thumb travels on a framer-motion `layout` animation instead of a CSS
 * `translate-x`, so the distance is read from the rendered track rather than
 * hard-coded — override the track width or the thumb size via `className` and
 * the motion follows. The thumb also squashes toward the side it is resting on
 * while the switch is held down, and springs back on release.
 *
 * `checkedIcon`/`uncheckedIcon` ride along inside the thumb: both are always
 * rendered and cross-fade — spinning and scaling past each other — off the
 * root's `data-state`, so no icon state has to be mirrored and the switch keeps
 * working uncontrolled.
 *
 * Honours `prefers-reduced-motion` by snapping instead of springing.
 */
function Switch({
  className,
  size = "default",
  checkedIcon,
  uncheckedIcon,
  onPointerDown,
  onPointerUp,
  onPointerLeave,
  onPointerCancel,
  disabled,
  ...props
}: React.ComponentProps<typeof SwitchPrimitive.Root> & {
  size?: "sm" | "default"
  /** Icon shown in the thumb while checked. */
  checkedIcon?: React.ReactNode
  /** Icon shown in the thumb while unchecked. */
  uncheckedIcon?: React.ReactNode
}) {
  const reduceMotion = useReducedMotion()
  const [pressed, setPressed] = React.useState(false)

  const transition = reduceMotion
    ? { duration: 0 }
    : { type: "spring" as const, stiffness: 520, damping: 32, mass: 0.6 }

  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      data-size={size}
      className={cn(
        "peer group/switch inline-flex shrink-0 items-center rounded-full border border-transparent shadow-xs transition-colors outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 data-[size=default]:h-[1.15rem] data-[size=default]:w-8 data-[size=sm]:h-3.5 data-[size=sm]:w-6 data-[state=checked]:bg-primary data-[state=unchecked]:bg-input dark:data-[state=unchecked]:bg-input/80 data-[state=checked]:justify-end data-[state=unchecked]:justify-start",
        className
      )}
      disabled={disabled}
      onPointerDown={(event) => {
        if (!disabled) setPressed(true)
        onPointerDown?.(event)
      }}
      onPointerUp={(event) => {
        setPressed(false)
        onPointerUp?.(event)
      }}
      onPointerLeave={(event) => {
        setPressed(false)
        onPointerLeave?.(event)
      }}
      onPointerCancel={(event) => {
        setPressed(false)
        onPointerCancel?.(event)
      }}
      {...props}
    >
      <SwitchPrimitive.Thumb asChild>
        <motion.span
          data-slot="switch-thumb"
          layout
          transition={transition}
          style={{ borderRadius: 9999 }}
          className="pointer-events-none relative block bg-background ring-0 group-data-[size=default]/switch:size-4 group-data-[size=sm]/switch:size-3 dark:group-data-[state=checked]/switch:bg-primary-foreground dark:group-data-[state=unchecked]/switch:bg-foreground"
        >
          {/* Inner layer carries the squash so it never fights the layout transform.
              It inherits its background so overriding the thumb's colour still works. */}
          <motion.span
            aria-hidden
            className="block size-full rounded-full bg-inherit group-data-[state=checked]/switch:origin-right group-data-[state=unchecked]/switch:origin-left"
            animate={pressed ? { scaleX: 1.18 } : { scaleX: 1 }}
            transition={transition}
          />
          {checkedIcon || uncheckedIcon ? (
            <span
              aria-hidden
              className="absolute inset-0 grid place-items-center [&>*]:col-start-1 [&>*]:row-start-1"
            >
              <span className={cn(iconLayer, "text-muted-foreground rotate-90 scale-50 opacity-0 group-data-[state=unchecked]/switch:rotate-0 group-data-[state=unchecked]/switch:scale-100 group-data-[state=unchecked]/switch:opacity-100")}>
                {uncheckedIcon}
              </span>
              <span className={cn(iconLayer, "text-primary -rotate-90 scale-50 opacity-0 group-data-[state=checked]/switch:rotate-0 group-data-[state=checked]/switch:scale-100 group-data-[state=checked]/switch:opacity-100")}>
                {checkedIcon}
              </span>
            </span>
          ) : null}
        </motion.span>
      </SwitchPrimitive.Thumb>
    </SwitchPrimitive.Root>
  )
}

export { Switch }
