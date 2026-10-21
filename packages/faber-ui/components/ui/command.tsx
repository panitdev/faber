"use client"

import * as React from "react"
import { Command as CommandPrimitive } from "cmdk"
import { AnimatePresence, animate, motion, useAnimationControls, useMotionValue } from "framer-motion"
import { ChevronRight, SearchIcon } from "lucide-react"
import { Dialog as DialogPrimitive } from "radix-ui"

import { cn } from "@charlotte-ui/utils"

// ─── NestContext ──────────────────────────────────────────────────────────────

type NestContextValue = {
  stack: string[]
  activeId: string | null
  direction: "forward" | "back"
  push: (id: string) => void
  pop: () => void
  reset: () => void
  activePlaceholder: string | undefined
  registerPlaceholder: (id: string, placeholder: string | undefined) => void
  searchValue: string
  setSearchValue: (v: string) => void
}

const NestContext = React.createContext<NestContextValue | null>(null)

function useNestContext(): NestContextValue | null {
  return React.useContext(NestContext)
}

// Tracks which nest ID is the immediate parent of a component's render position.
// Null = root level (outside any CommandNest container).
const ParentNestIdContext = React.createContext<string | null>(null)

function useParentNestId(): string | null {
  return React.useContext(ParentNestIdContext)
}

function useCommandSearch(): string {
  const ctx = useNestContext()
  return ctx?.searchValue ?? ""
}

function NestProvider({ children }: { children: React.ReactNode }) {
  const [stack, setStack] = React.useState<string[]>([])
  const [direction, setDirection] = React.useState<"forward" | "back">("forward")
  const [searchValue, setSearchValueState] = React.useState("")
  const placeholders = React.useRef<Map<string, string | undefined>>(new Map())

  const setSearchValue = React.useCallback((v: string) => setSearchValueState(v), [])

  const push = React.useCallback((id: string) => {
    setDirection("forward")
    setStack((s) => [...s, id])
    setSearchValueState("")
  }, [])

  const pop = React.useCallback(() => {
    setDirection("back")
    setStack((s) => s.slice(0, -1))
    setSearchValueState("")
  }, [])

  const reset = React.useCallback(() => {
    setStack([])
    setDirection("forward")
    setSearchValueState("")
  }, [])

  const registerPlaceholder = React.useCallback(
    (id: string, placeholder: string | undefined) => {
      placeholders.current.set(id, placeholder)
    },
    []
  )

  const activeId = stack.length > 0 ? stack[stack.length - 1] : null
  const activePlaceholder = activeId ? placeholders.current.get(activeId) : undefined

  return (
    <NestContext.Provider
      value={{
        stack,
        activeId,
        direction,
        push,
        pop,
        reset,
        activePlaceholder,
        registerPlaceholder,
        searchValue,
        setSearchValue,
      }}
    >
      {children}
    </NestContext.Provider>
  )
}

// ─── Command ──────────────────────────────────────────────────────────────────

function Command({
  className,
  ...props
}: React.ComponentProps<typeof CommandPrimitive>) {
  return (
    <CommandPrimitive
      data-slot="command"
      className={cn(
        "flex flex-col overflow-hidden rounded-xl border border-border bg-popover text-popover-foreground shadow-lg",
        className
      )}
      {...props}
    />
  )
}

// ─── CommandDialog ────────────────────────────────────────────────────────────

function CommandDialog({
  title = "Command Palette",
  description = "Search for a command to run...",
  children,
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Root> & {
  title?: string
  description?: string
  className?: string
}) {
  return (
    <NestProvider>
      <CommandDialogInner
        title={title}
        description={description}
        className={className}
        {...props}
      >
        {children}
      </CommandDialogInner>
    </NestProvider>
  )
}

function CommandDialogInner({
  title,
  description,
  children,
  className,
  onOpenChange,
  open: openProp,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Root> & {
  title?: string
  description?: string
  className?: string
}) {
  const ctx = useNestContext()
  const squishControls = useAnimationControls()
  const squishInitialized = React.useRef(false)

  // Reset nest state when dialog opens, so it always starts at the root.
  // We intentionally do NOT reset on close — the closing animation
  // plays out with the current nest page visible (natural fade-out),
  // and the state is cleared before the next open.
  const resetRef = React.useRef(ctx?.reset)
  resetRef.current = ctx?.reset
  React.useEffect(() => {
    if (openProp) {
      resetRef.current?.()
      squishInitialized.current = false
    }
  }, [openProp])

  // Scale-compress the whole dialog box on each navigation, then spring back.
  // Uses asChild + motion.div so framer owns the transform stack and composes
  // scale with the existing translate-x-1/2 — plain animate(el) would clobber it.
  React.useEffect(() => {
    if (!squishInitialized.current) {
      squishInitialized.current = true
      return
    }
    squishControls.start({
      scale: [1, 0.97, 1],
      transition: { duration: 0.35, times: [0, 0.12, 1], ease: ["linear", [0.22, 1, 0.36, 1]] },
    })
  }, [ctx?.activeId])

  function handleOpenChange(next: boolean) {
    // Reset is handled by the useEffect above watching openProp,
    // which catches both programmatic and user-initiated closes.
    onOpenChange?.(next)
  }

  return (
    <DialogPrimitive.Root open={openProp} onOpenChange={handleOpenChange} {...props}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/50 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0" />
        <DialogPrimitive.Content asChild>
          <motion.div
            animate={squishControls}
            className={cn(
              "fixed top-1/3 left-1/2 z-50 w-full max-w-lg -translate-x-1/2 overflow-hidden rounded-xl border border-border bg-popover shadow-lg outline-none data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
              className
            )}
          >
            <DialogPrimitive.Title className="sr-only">{title}</DialogPrimitive.Title>
            <DialogPrimitive.Description className="sr-only">{description}</DialogPrimitive.Description>
            {children}
          </motion.div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}

// ─── CommandInput ─────────────────────────────────────────────────────────────

function CommandInput({
  className,
  onKeyDown,
  placeholder,
  value: valueProp,
  onValueChange: onValueChangeProp,
  ...props
}: React.ComponentProps<typeof CommandPrimitive.Input>) {
  const ctx = useNestContext()

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (
      ctx &&
      e.key === "Backspace" &&
      e.currentTarget.value === "" &&
      ctx.stack.length > 0
    ) {
      e.preventDefault()
      ctx.pop()
    }
    onKeyDown?.(e)
  }

  return (
    <div
      data-slot="command-input-wrapper"
      className="flex items-center gap-3 px-4"
    >
      <SearchIcon className="size-5 shrink-0 text-muted-foreground" aria-hidden />
      <CommandPrimitive.Input
        data-slot="command-input"
        className={cn(
          "flex h-14 w-full bg-transparent py-3 text-base outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50",
          className
        )}
        value={ctx ? ctx.searchValue : valueProp}
        onValueChange={ctx ? ctx.setSearchValue : onValueChangeProp}
        placeholder={ctx?.activePlaceholder ?? placeholder}
        onKeyDown={handleKeyDown}
        {...props}
      />
    </div>
  )
}

// ─── CommandList ──────────────────────────────────────────────────────────────

function CommandList({
  className,
  style,
  maxHeight = 320,
  children,
  ...props
}: React.ComponentProps<typeof CommandPrimitive.List> & {
  maxHeight?: number
}) {
  const listRef = React.useRef<HTMLDivElement>(null)
  const motionHeight = useMotionValue(0)
  const initialized = React.useRef(false)
  const ctx = useNestContext()

  React.useEffect(() => {
    const el = listRef.current
    if (!el) return

    const observer = new MutationObserver(() => {
      const px = parseFloat(el.style.getPropertyValue("--cmdk-list-height") || "0")
      if (!Number.isFinite(px) || px <= 0) return
      const target = Math.min(px, maxHeight)
      if (!initialized.current) {
        motionHeight.jump(target)
        initialized.current = true
      } else {
        animate(motionHeight, target, { type: "spring", stiffness: 350, damping: 30 })
      }
    })

    observer.observe(el, { attributes: true, attributeFilter: ["style"] })
    return () => observer.disconnect()
  }, [maxHeight, motionHeight])

  const content = ctx ? (
    <AnimatePresence initial={false}>
      <motion.div
        key={ctx.activeId ?? "root"}
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: 0.12, ease: "linear" }}
      >
        {children}
      </motion.div>
    </AnimatePresence>
  ) : (
    children
  )

  return (
    <motion.div style={{ height: motionHeight, overflow: "hidden" }}>
      <CommandPrimitive.List
        ref={listRef}
        data-slot="command-list"
        className={cn(
          "h-full overflow-y-auto overflow-x-hidden [&::-webkit-scrollbar]:hidden",
          className
        )}
        style={{ ...style, scrollbarWidth: "none" } as React.CSSProperties}
        {...props}
      >
        {content}
      </CommandPrimitive.List>
    </motion.div>
  )
}

// ─── CommandEmpty ─────────────────────────────────────────────────────────────

function CommandEmpty({
  className,
  ...props
}: React.ComponentProps<typeof CommandPrimitive.Empty>) {
  return (
    <CommandPrimitive.Empty
      data-slot="command-empty"
      className={cn("py-6 text-center text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

// ─── CommandGroup ─────────────────────────────────────────────────────────────

function CommandGroup({
  className,
  children,
  ...props
}: React.ComponentProps<typeof CommandPrimitive.Group>) {
  const ctx = useNestContext()
  const parentId = useParentNestId()

  // When this group is not in the active nest level, strip the cmdk group
  // wrapper (heading + registration) but keep children in the React tree so
  // that any CommandNest containers nested inside can stay mounted.
  if (ctx && ctx.activeId !== parentId) {
    return <>{children}</>
  }

  return (
    <CommandPrimitive.Group
      data-slot="command-group"
      className={cn(
        "overflow-hidden py-1.5 text-foreground [&_[cmdk-group-heading]]:px-4 [&_[cmdk-group-heading]]:pb-1.5 [&_[cmdk-group-heading]]:pt-2.5 [&_[cmdk-group-heading]]:text-sm [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-muted-foreground",
        className
      )}
      {...props}
    >
      {children}
    </CommandPrimitive.Group>
  )
}

// ─── CommandSeparator ─────────────────────────────────────────────────────────

function CommandSeparator({
  className,
  ...props
}: React.ComponentProps<typeof CommandPrimitive.Separator>) {
  return (
    <CommandPrimitive.Separator
      data-slot="command-separator"
      className={cn("my-1 h-px bg-border", className)}
      {...props}
    />
  )
}

// ─── CommandItem ──────────────────────────────────────────────────────────────

function CommandItem({
  className,
  ...props
}: React.ComponentProps<typeof CommandPrimitive.Item>) {
  const ctx = useNestContext()
  const parentId = useParentNestId()

  // Hide items that are not in the currently active nest level.
  if (ctx && ctx.activeId !== parentId) return null

  return (
    <CommandPrimitive.Item
      data-slot="command-item"
      className={cn(
        "relative mx-2 flex cursor-default select-none items-center gap-3 rounded-lg px-4 py-3 text-base outline-none aria-selected:bg-accent aria-selected:text-accent-foreground data-[disabled=true]:pointer-events-none data-[disabled=true]:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-5 [&_svg]:shrink-0 [&_svg]:text-muted-foreground aria-selected:[&_svg]:text-accent-foreground",
        className
      )}
      {...props}
    />
  )
}

// ─── CommandShortcut ──────────────────────────────────────────────────────────

function CommandShortcut({
  className,
  ...props
}: React.ComponentProps<"span">) {
  return (
    <span
      data-slot="command-shortcut"
      className={cn("ml-auto flex shrink-0 items-center gap-1", className)}
      {...props}
    />
  )
}

// ─── CommandKbd ───────────────────────────────────────────────────────────────

function CommandKbd({ className, ...props }: React.ComponentProps<"kbd">) {
  return (
    <kbd
      className={cn(
        "flex h-5 min-w-5 items-center justify-center rounded border border-border bg-muted px-1 font-mono text-[10px] font-medium text-muted-foreground",
        className
      )}
      {...props}
    />
  )
}

// ─── CommandNest ──────────────────────────────────────────────────────────────

interface CommandNestProps {
  label: string
  icon?: React.ReactNode
  placeholder?: string
  children: React.ReactNode
  shortcut?: React.ReactNode
  keywords?: string[]
}

function CommandNest({
  label,
  icon,
  placeholder,
  children,
  shortcut,
  keywords,
}: CommandNestProps) {
  const ctx = useNestContext()
  const parentId = useParentNestId()

  // Stable identity: prefix the label with the parent nest's ID so the same
  // label at different nesting levels doesn't collide in the flat stack.
  // Props-based (not useId) so it survives AnimatePresence unmount/remount
  // when CommandList's key changes on navigation.
  const id = parentId ? `${parentId}>${label}` : label

  React.useEffect(() => {
    ctx?.registerPlaceholder(id, placeholder)
  }, [id, placeholder, ctx])

  if (!ctx) return null

  const { stack, activeId, push } = ctx

  // Show the trigger item when the parent nest level is active.
  const isActiveTrigger = activeId === parentId

  // Keep the container mounted for this nest and all its ancestors on the
  // navigation path, so deeply-nested CommandNest trees stay in the React tree.
  const isOnStack = stack.includes(id)

  return (
    <>
      {isActiveTrigger && (
        <CommandItem value={label} keywords={keywords} onSelect={() => push(id)}>
          {icon}
          <span>{label}</span>
          <CommandShortcut>
            {shortcut ?? <ChevronRight className="!text-muted-foreground" />}
          </CommandShortcut>
        </CommandItem>
      )}

      {isOnStack && (
        <ParentNestIdContext.Provider value={id}>
          {children}
        </ParentNestIdContext.Provider>
      )}
    </>
  )
}

export {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandKbd,
  CommandList,
  CommandNest,
  CommandSeparator,
  CommandShortcut,
  useCommandSearch,
}
export type { CommandNestProps }
