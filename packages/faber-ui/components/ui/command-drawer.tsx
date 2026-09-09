"use client"

import * as React from "react"
import { animate, motion, useMotionTemplate, useMotionValue } from "framer-motion"
import { ChevronLeft } from "lucide-react"
import { Drawer } from "vaul"

import { ActionRow, ActionRowGroup, type ActionRowProps } from "@/components/ui/action-row"
import { cn } from "@/lib/utils"

// ─── NestContext ──────────────────────────────────────────────────────────

type NestContextValue = {
  stack: string[]
  activeId: string | null
  direction: "forward" | "back"
  push: (id: string) => void
  pop: () => void
  reset: () => void
  activeTitle: string | undefined
  registerTitle: (id: string, title: string | undefined) => void
}

const NestContext = React.createContext<NestContextValue | null>(null)

function useNestContext(): NestContextValue | null {
  return React.useContext(NestContext)
}

// Tracks which nest ID is the immediate parent of a component's render
// position. Null = root level (outside any CommandDrawerNest container).
const ParentNestIdContext = React.createContext<string | null>(null)

function useParentNestId(): string | null {
  return React.useContext(ParentNestIdContext)
}

function NestProvider({ children }: { children: React.ReactNode }) {
  const [stack, setStack] = React.useState<string[]>([])
  const [direction, setDirection] = React.useState<"forward" | "back">("forward")
  const titles = React.useRef<Map<string, string | undefined>>(new Map())

  const push = React.useCallback((id: string) => {
    setDirection("forward")
    setStack((s) => [...s, id])
  }, [])

  const pop = React.useCallback(() => {
    setDirection("back")
    setStack((s) => s.slice(0, -1))
  }, [])

  const reset = React.useCallback(() => {
    setStack([])
    setDirection("forward")
  }, [])

  const registerTitle = React.useCallback(
    (id: string, title: string | undefined) => {
      titles.current.set(id, title)
    },
    [],
  )

  const activeId = stack.length > 0 ? stack[stack.length - 1] : null
  const activeTitle = activeId ? titles.current.get(activeId) : undefined

  return (
    <NestContext.Provider
      value={{
        stack,
        activeId,
        direction,
        push,
        pop,
        reset,
        activeTitle,
        registerTitle,
      }}
    >
      {children}
    </NestContext.Provider>
  )
}

// ─── DisplayContext ───────────────────────────────────────────────────────
// Provides a frozen snapshot of activeId/stack that lags behind NestContext
// during crossfade animations. Children read from here so the exiting frame
// always renders the correct stale content, and the entering frame shows new
// content only after the fade-out completes.

type DisplayState = { activeId: string | null; stack: string[] }

const DisplayContext = React.createContext<DisplayState>({ activeId: null, stack: [] })

function useDisplayActiveId(): string | null {
  return React.useContext(DisplayContext).activeId
}

function useDisplayStack(): string[] {
  return React.useContext(DisplayContext).stack
}

// ─── CommandDrawer (Root) ─────────────────────────────────────────────────

function CommandDrawer({
  children,
  ...props
}: React.ComponentProps<typeof Drawer.Root>) {
  return (
    <NestProvider>
      <CommandDrawerInner {...props}>{children}</CommandDrawerInner>
    </NestProvider>
  )
}

function CommandDrawerInner({
  open: openProp,
  children,
  ...props
}: React.ComponentProps<typeof Drawer.Root>) {
  const ctx = useNestContext()

  // Reset nest state when the drawer opens so it always starts at root.
  const resetRef = React.useRef(ctx?.reset)
  resetRef.current = ctx?.reset
  React.useEffect(() => {
    if (openProp) {
      resetRef.current?.()
    }
  }, [openProp])

  return (
    <Drawer.Root open={openProp} {...props}>
      {children}
    </Drawer.Root>
  )
}

// ─── CommandDrawerTrigger ─────────────────────────────────────────────────

function CommandDrawerTrigger({
  children,
  ...props
}: React.ComponentProps<typeof Drawer.Trigger>) {
  return <Drawer.Trigger {...props}>{children}</Drawer.Trigger>
}

// ─── CommandDrawerContent ─────────────────────────────────────────────────

interface CommandDrawerContentProps {
  /** Root-level header label shown when no nest is active. */
  title?: string
  /** sr-only description for accessibility (Drawer.Description). */
  description?: string
  /** Maximum height of the animated body in px (default 420). */
  maxHeight?: number
  className?: string
  children: React.ReactNode
}

function CommandDrawerContent({
  title,
  description = "Navigation menu",
  maxHeight = 420,
  className,
  children,
}: CommandDrawerContentProps) {
  const ctx = useNestContext()
  const motionHeight = useMotionValue(0)
  const initialized = React.useRef(false)
  const [heightReady, setHeightReady] = React.useState(false)

  // ResizeObserver-driven spring height.
  //
  // Uses a callback ref (not useRef + useEffect) because Vaul renders
  // Drawer.Content's children asynchronously — the observed <div> may
  // not exist in the DOM when the component first mounts.

  const [measureNode, setMeasureNode] = React.useState<HTMLDivElement | null>(null)

  const measureCallbackRef = React.useCallback(
    (node: HTMLDivElement | null) => setMeasureNode(node),
    [],
  )

  React.useEffect(() => {
    const el = measureNode
    if (!el) return

    const observer = new ResizeObserver((entries) => {
      const naturalHeight = entries[0].contentRect.height
      if (!Number.isFinite(naturalHeight) || naturalHeight <= 0) return

      const target = Math.min(naturalHeight + 32, maxHeight)

      if (!initialized.current) {
        motionHeight.jump(target)
        initialized.current = true
        setHeightReady(true)
      } else {
        animate(motionHeight, target, {
          type: "spring",
          stiffness: 350,
          damping: 30,
        })
      }
    })

    observer.observe(el)
    return () => observer.disconnect()
  }, [maxHeight, measureNode])

  // ── Crossfade ────────────────────────────────────────────────────────
  // Simultaneous crossfade: new content fades in while old content (captured
  // as exitingState) fades out on top via an absolutely-positioned overlay.

  const activeId = ctx?.activeId ?? null
  const stack = ctx?.stack ?? []

  const [displayState, setDisplayState] = React.useState<DisplayState>({
    activeId: null,
    stack: [],
  })

  const [exitingState, setExitingState] = React.useState<DisplayState | null>(null)

  // enterOpacity: imperative animation works because the entering frame is
  // always mounted. exitOpacity was dropped — the exiting frame is conditionally
  // mounted, so any imperative animate() call fires before the subscriber
  // exists. The exiting frame uses declarative initial/animate instead.
  const enterOpacity = useMotionValue(1)
  const enterBlur = useMotionValue(0)
  const enterFilter = useMotionTemplate`blur(${enterBlur}px)`

  const latestCtxRef = React.useRef({ activeId, stack })
  React.useEffect(() => {
    latestCtxRef.current = { activeId, stack }
  })

  const crossfadeTimerRef = React.useRef<ReturnType<typeof setTimeout> | null>(null)

  React.useEffect(() => {
    // displayState is intentionally not in deps: the closure captures the
    // correct value for this render, and re-running on setDisplayState would
    // cancel in-flight timers prematurely.
    if (activeId === displayState.activeId) return

    if (crossfadeTimerRef.current) {
      clearTimeout(crossfadeTimerRef.current)
      crossfadeTimerRef.current = null
    }

    const to = { activeId: latestCtxRef.current.activeId, stack: latestCtxRef.current.stack }

    setDisplayState(to)
    setExitingState(displayState)
    enterOpacity.jump(0)
    enterBlur.jump(3)
    animate(enterOpacity, 1, { duration: 0.12, ease: "linear" })
    animate(enterBlur, 0, { duration: 0.12, ease: "linear" })

    crossfadeTimerRef.current = setTimeout(() => {
      crossfadeTimerRef.current = null
      setExitingState(null)
    }, 120)

    return () => {
      if (crossfadeTimerRef.current) {
        clearTimeout(crossfadeTimerRef.current)
        crossfadeTimerRef.current = null
      }
    }
  }, [activeId])

  const showHeader = (ctx?.stack.length ?? 0) > 0 || title !== undefined

  return (
    <Drawer.Portal>
      <Drawer.Overlay className="fixed inset-0 z-50 bg-black/50" />
      <Drawer.Content
        className={cn(
          "fixed inset-x-0 bottom-0 z-50 flex h-auto max-h-[90dvh] flex-col rounded-t-[calc(var(--radius)+4px)] border bg-background focus:outline-none",
          className,
        )}
      >
        {/* Drag handle */}
        <div
          aria-hidden="true"
          className="mx-auto mt-3.5 h-1.5 w-12 shrink-0 rounded-full bg-muted-foreground/25"
        />

        {/* Header — back chevron + active nest title */}
        {showHeader && (
          <div className="flex items-center gap-2 border-b border-border/50 px-4 py-3">
            <button
              type="button"
              onClick={() => ctx?.pop()}
              aria-label="Go back"
              className={cn(
                "-ml-1 flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                (ctx?.stack.length ?? 0) === 0 && "invisible",
              )}
            >
              <ChevronLeft className="size-5" />
            </button>
            <span className="truncate text-sm font-medium">
              {ctx?.activeTitle ?? title}
            </span>
          </div>
        )}

        {/* sr-only a11y nodes so vaul doesn't warn about missing Title/Description */}
        <Drawer.Title className="sr-only">{title ?? "Menu"}</Drawer.Title>
        <Drawer.Description className="sr-only">{description}</Drawer.Description>

        {/* Animated-height body */}
        {/* position:relative so the exiting frame can escape the scrollable
            div and be clipped only by this outer wrapper, not overflow-y-auto. */}
        <motion.div
          style={heightReady ? { height: motionHeight, flexShrink: 0, position: "relative" } : { position: "relative" }}
        >
          {/* Exiting frame: sits outside the scroll container so overflow-y-auto
              doesn't clip old content that's taller than the entering content. */}
          {exitingState && (
            <DisplayContext.Provider value={exitingState}>
              <motion.div
                aria-hidden
                initial={{ opacity: 1, filter: "blur(0px)" }}
                animate={{ opacity: 0, filter: "blur(3px)" }}
                transition={{ duration: 0.12, ease: "linear" }}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  right: 0,
                  pointerEvents: "none",
                  zIndex: 1,
                }}
              >
                {children}
              </motion.div>
            </DisplayContext.Provider>
          )}
          <div
            className="overflow-y-auto [&::-webkit-scrollbar]:hidden"
            style={{ maxHeight, scrollbarWidth: "none" } as React.CSSProperties}
          >
            {/* Entering frame: new content in normal flow, fades in.
                measureCallbackRef lives here so height only tracks entering content. */}
            <DisplayContext.Provider value={displayState}>
              <motion.div style={{ opacity: enterOpacity, filter: enterFilter }}>
                <div ref={measureCallbackRef}>
                  {children}
                </div>
              </motion.div>
            </DisplayContext.Provider>
          </div>
        </motion.div>
      </Drawer.Content>
    </Drawer.Portal>
  )
}

// ─── CommandDrawerGroup ───────────────────────────────────────────────────

function CommandDrawerGroup({
  className,
  children,
  ...props
}: React.ComponentProps<"div">) {
  const ctx = useNestContext()
  const parentId = useParentNestId()
  const displayActiveId = useDisplayActiveId()

  // When this group is not in the active nest level, strip the card wrapper
  // but keep children mounted so nested CommandDrawerNest containers survive.
  if (ctx && displayActiveId !== parentId) {
    return <>{children}</>
  }

  return (
    <ActionRowGroup
      data-slot="command-drawer-group"
      className={cn("mx-3 my-3", className)}
      {...props}
    >
      {children}
    </ActionRowGroup>
  )
}

// ─── CommandDrawerItem ────────────────────────────────────────────────────

type CommandDrawerItemProps = ActionRowProps

/**
 * An {@link ActionRow} that is aware of the drawer's nest stack: it renders only
 * while the level it was declared in is the one on screen.
 */
function CommandDrawerItem(props: CommandDrawerItemProps) {
  const ctx = useNestContext()
  const parentId = useParentNestId()
  const displayActiveId = useDisplayActiveId()

  // Hide items that are not in the currently displayed nest level.
  if (ctx && displayActiveId !== parentId) return null

  return <ActionRow data-slot="command-drawer-item" {...props} />
}

// ─── CommandDrawerNest ────────────────────────────────────────────────────

interface CommandDrawerNestProps {
  label: string
  icon?: React.ReactNode
  description?: string
  iconClassName?: string
  children: React.ReactNode
}

function CommandDrawerNest({
  label,
  icon,
  description,
  iconClassName,
  children,
}: CommandDrawerNestProps) {
  const ctx = useNestContext()
  const parentId = useParentNestId()
  const displayActiveId = useDisplayActiveId()
  const displayStack = useDisplayStack()

  // Stable identity: prefix the label with the parent nest's ID so the same
  // label at different nesting levels doesn't collide.
  const id = parentId ? `${parentId}>${label}` : label

  React.useEffect(() => {
    ctx?.registerTitle(id, label)
  }, [id, label, ctx])

  if (!ctx) return null

  // Show the trigger item when the displayed nest level matches the parent.
  const isActiveTrigger = displayActiveId === parentId

  // Keep the container mounted for this nest and all its ancestors on the
  // displayed navigation path, so deeply-nested trees stay in the React tree.
  const isOnStack = displayStack.includes(id)

  return (
    <>
      {isActiveTrigger && (
        <CommandDrawerItem
          icon={icon}
          label={label}
          description={description}
          iconClassName={iconClassName}
          chevron
          onSelect={() => ctx.push(id)}
        />
      )}

      {isOnStack && (
        <ParentNestIdContext.Provider value={id}>
          {children}
        </ParentNestIdContext.Provider>
      )}
    </>
  )
}

// ─── Exports ──────────────────────────────────────────────────────────────

export {
  CommandDrawer,
  CommandDrawerTrigger,
  CommandDrawerContent,
  CommandDrawerGroup,
  CommandDrawerItem,
  CommandDrawerNest,
}
export type { CommandDrawerItemProps, CommandDrawerNestProps }
