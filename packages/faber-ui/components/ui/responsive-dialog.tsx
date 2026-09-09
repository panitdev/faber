"use client"

import * as React from "react"
import { Dialog as DialogPrimitive } from "radix-ui"
import { Drawer } from "vaul"
import { XIcon } from "lucide-react"

import { cn } from "@/lib/utils"

// ─── Mobile detection ─────────────────────────────────────────────────────────

function useIsMobile(breakpoint = 640): boolean {
  const [isMobile, setIsMobile] = React.useState(false)

  React.useEffect(() => {
    const mq = window.matchMedia(`(max-width: ${breakpoint - 1}px)`)
    setIsMobile(mq.matches)
    const onChange = (e: MediaQueryListEvent) => setIsMobile(e.matches)
    mq.addEventListener("change", onChange)
    return () => mq.removeEventListener("change", onChange)
  }, [breakpoint])

  return isMobile
}

// ─── Context ──────────────────────────────────────────────────────────────────

const ResponsiveDialogContext = React.createContext<{ isMobile: boolean }>({
  isMobile: false,
})

// ─── Dialog (Root) ────────────────────────────────────────────────────────────

type DialogProps = React.ComponentProps<typeof DialogPrimitive.Root> & {
  /** Scale the background when the drawer opens — requires vaul body wrapper (mobile only) */
  shouldScaleBackground?: boolean
  /** Allow dismissing the drawer by dragging or tapping outside (mobile only) */
  dismissible?: boolean
  /** Snap point positions as percentages or pixel values (mobile only) */
  snapPoints?: (string | number)[]
  /** Slide-in direction for the drawer (mobile only) */
  direction?: "top" | "bottom" | "left" | "right"
}

function Dialog({
  children,
  modal,
  shouldScaleBackground = false,
  dismissible = true,
  snapPoints,
  direction = "bottom",
  ...props
}: DialogProps) {
  const isMobile = useIsMobile()

  return (
    <ResponsiveDialogContext.Provider value={{ isMobile }}>
      {isMobile ? (
        <Drawer.Root
          {...props}
          shouldScaleBackground={shouldScaleBackground}
          dismissible={dismissible}
          snapPoints={snapPoints}
          direction={direction}
        >
          {children}
        </Drawer.Root>
      ) : (
        <DialogPrimitive.Root data-slot="dialog" {...props} modal={modal}>
          {children}
        </DialogPrimitive.Root>
      )}
    </ResponsiveDialogContext.Provider>
  )
}

// ─── DialogTrigger ────────────────────────────────────────────────────────────

function DialogTrigger({
  children,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Trigger>) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return (
      <Drawer.Trigger data-slot="dialog-trigger" {...props}>
        {children}
      </Drawer.Trigger>
    )
  }
  return (
    <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props}>
      {children}
    </DialogPrimitive.Trigger>
  )
}

// ─── DialogClose ──────────────────────────────────────────────────────────────

function DialogClose({
  children,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Close>) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return (
      <Drawer.Close data-slot="dialog-close" {...props}>
        {children}
      </Drawer.Close>
    )
  }
  return (
    <DialogPrimitive.Close data-slot="dialog-close" {...props}>
      {children}
    </DialogPrimitive.Close>
  )
}

// ─── DialogPortal ─────────────────────────────────────────────────────────────

function DialogPortal({
  children,
  container,
}: React.ComponentProps<typeof DialogPrimitive.Portal>) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return <Drawer.Portal container={container}>{children}</Drawer.Portal>
  }
  return (
    <DialogPrimitive.Portal data-slot="dialog-portal" container={container}>
      {children}
    </DialogPrimitive.Portal>
  )
}

// ─── DialogOverlay ────────────────────────────────────────────────────────────

function DialogOverlay({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Overlay>) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return (
      <Drawer.Overlay
        data-slot="dialog-overlay"
        className={cn("fixed inset-0 z-50 bg-black/50", className)}
      // Drawer.Overlay doesn't accept all Radix Overlay props — pass only className
      />
    )
  }
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 z-50 bg-black/50 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0",
        className
      )}
      {...props}
    />
  )
}

// ─── DialogContent ────────────────────────────────────────────────────────────

type DialogContentProps = React.ComponentProps<typeof DialogPrimitive.Content> & {
  /** Show the built-in ✕ close button (default: true) */
  showCloseButton?: boolean
  /** Extra class applied only to the outer drawer shell (mobile only) */
  drawerContentClassName?: string
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  drawerContentClassName,
  // Extract Dialog-specific props that Drawer.Content doesn't support
  onOpenAutoFocus,
  onCloseAutoFocus,
  forceMount,
  onEscapeKeyDown,
  onInteractOutside,
  onPointerDownOutside,
  ...props
}: DialogContentProps) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return (
      <Drawer.Portal>
        <Drawer.Overlay className="fixed inset-0 z-50 bg-black/50" />
        <Drawer.Content
          data-slot="dialog-content"
          onEscapeKeyDown={onEscapeKeyDown}
          // vaul uses the same Radix DismissableLayer types under the hood
          onInteractOutside={
            onInteractOutside as React.ComponentProps<
              typeof Drawer.Content
            >["onInteractOutside"]
          }
          onPointerDownOutside={
            onPointerDownOutside as React.ComponentProps<
              typeof Drawer.Content
            >["onPointerDownOutside"]
          }
          className={cn(
            "fixed inset-x-0 bottom-0 z-50 flex h-auto max-h-[90dvh] flex-col rounded-t-[calc(var(--radius)+4px)] border bg-background focus:outline-none",
            drawerContentClassName
          )}
        >
          {/* Drag handle */}
          <div
            aria-hidden="true"
            className="mx-auto mt-3.5 h-1.5 w-12 shrink-0 rounded-full bg-muted-foreground/25"
          />
          {showCloseButton && (
            <Drawer.Close
              data-slot="dialog-close"
              className="absolute top-3.5 right-4 rounded-xs opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:ring-2 focus:ring-ring focus:ring-offset-2 focus:outline-hidden disabled:pointer-events-none data-[state=open]:bg-accent data-[state=open]:text-muted-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"
            >
              <XIcon />
              <span className="sr-only">Close</span>
            </Drawer.Close>
          )}
          {/* Scrollable body */}
          <div className={cn("flex flex-col gap-4 overflow-y-auto px-6 pb-6 pt-4", className)}>
            {children}
          </div>
        </Drawer.Content>
      </Drawer.Portal>
    )
  }

  return (
    <DialogPortal data-slot="dialog-portal">
      <DialogOverlay />
      <DialogPrimitive.Content
        data-slot="dialog-content"
        onEscapeKeyDown={onEscapeKeyDown}
        onInteractOutside={onInteractOutside}
        onPointerDownOutside={onPointerDownOutside}
        onOpenAutoFocus={onOpenAutoFocus}
        onCloseAutoFocus={onCloseAutoFocus}
        forceMount={forceMount}
        className={cn(
          "fixed top-[50%] left-[50%] z-50 grid w-full max-w-[calc(100%-2rem)] translate-x-[-50%] translate-y-[-50%] gap-4 rounded-3xl border bg-background p-6 shadow-lg duration-200 outline-none data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95 sm:max-w-lg",
          className
        )}
        {...props}
      >
        {children}
        {showCloseButton && (
          <DialogPrimitive.Close
            data-slot="dialog-close"
            className="absolute top-4 right-4 rounded-xs opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:ring-2 focus:ring-ring focus:ring-offset-2 focus:outline-hidden disabled:pointer-events-none data-[state=open]:bg-accent data-[state=open]:text-muted-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"
          >
            <XIcon />
            <span className="sr-only">Close</span>
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Content>
    </DialogPortal>
  )
}

// ─── DialogHeader ─────────────────────────────────────────────────────────────

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      className={cn("flex flex-col gap-2 text-center sm:text-left", className)}
      {...props}
    />
  )
}

// ─── DialogFooter ─────────────────────────────────────────────────────────────

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  /** Append a styled close button as the first action (default: false) */
  showCloseButton?: boolean
}) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", className)}
      {...props}
    >
      {children}
      {showCloseButton && (
        <DialogClose asChild>
          <button className="inline-flex h-9 items-center justify-center rounded-lg border border-border bg-card px-4 text-sm font-medium text-foreground transition-colors hover:border-foreground/30 focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50">
            Close
          </button>
        </DialogClose>
      )}
    </div>
  )
}

// ─── DialogTitle ──────────────────────────────────────────────────────────────

function DialogTitle({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Title>) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return (
      <Drawer.Title
        data-slot="dialog-title"
        className={cn("text-lg leading-none font-semibold", className)}
        {...props}
      />
    )
  }
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn("text-lg leading-none font-semibold", className)}
      {...props}
    />
  )
}

// ─── DialogDescription ────────────────────────────────────────────────────────

function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  const { isMobile } = React.useContext(ResponsiveDialogContext)

  if (isMobile) {
    return (
      <Drawer.Description
        data-slot="dialog-description"
        className={cn("text-sm text-muted-foreground", className)}
        {...props}
      />
    )
  }
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
  DialogTrigger,
}
