import * as React from "react"
import { Loader2 } from "lucide-react"
import { AnimatePresence, animate, motion, useMotionValue } from "framer-motion"
import { Slot } from "radix-ui"
import { cva, type VariantProps } from "class-variance-authority"

import { cn } from "@/lib/utils"

const buttonVariants = cva(
  "relative inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-full border text-sm font-medium select-none outline-none transition-colors disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0 focus-visible:ring-[3px] focus-visible:ring-ring/50 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40",
  {
    variants: {
      variant: {
        default:
          "border-none bg-primary text-primary-foreground shadow-[0_1px_0_0_oklch(1_0_0_/_0.15)_inset,0_8px_20px_-8px_color-mix(in_oklab,var(--primary)_55%,transparent)] hover:bg-primary/90",
        destructive:
          "border-none bg-destructive text-white shadow-[0_1px_0_0_oklch(1_0_0_/_0.12)_inset,0_8px_20px_-8px_color-mix(in_oklab,var(--destructive)_45%,transparent)] hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:focus-visible:ring-destructive/40",
        outline:
          "border-border bg-card text-foreground hover:border-foreground/30",
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

type ButtonProps = React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean
    loading?: boolean
    loadingText?: React.ReactNode
    animateWidth?: boolean
  }

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
  ...props
}: ButtonProps) {
  const isDisabled = disabled || loading

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
      whileTap={isDisabled ? undefined : { scale: 0.97 }}
      whileHover={isDisabled ? undefined : { y: -1 }}
      transition={{ type: "spring", stiffness: 600, damping: 30 }}
      style={{ ...externalStyle, ...(animateWidth ? { width: animatedWidth } : {}) }}
      className={cn(buttonVariants({ variant, size, fullWidth, className }), "overflow-hidden")}
      disabled={isDisabled}
      onClick={onClick}
      {...restProps}
    >
      {content}
    </motion.button>
  )
}

export { Button, buttonVariants }
