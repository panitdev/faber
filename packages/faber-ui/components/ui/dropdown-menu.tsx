"use client"

import * as React from "react"
import {
  ArrowLeftIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CircleIcon,
} from "lucide-react"
import { AnimatePresence, motion, useReducedMotion } from "framer-motion"
import { DropdownMenu as DropdownMenuPrimitive } from "radix-ui"

import { buttonVariants } from "@/components/ui/button"
import { cn } from "@/lib/utils"

// ─── Motion ───────────────────────────────────────────────────────────────

const PANEL_TRANSITION = {
  duration: 0.22,
  ease: [0.32, 0.72, 0, 1],
} as const

const HEIGHT_TRANSITION = `height ${PANEL_TRANSITION.duration}s cubic-bezier(${PANEL_TRANSITION.ease.join(",")})`

const TRIGGER_TRANSITION = ["width", "margin-inline-start", "margin-inline-end"]
  .map(
    (property) =>
      `${property} ${PANEL_TRANSITION.duration}s cubic-bezier(${PANEL_TRANSITION.ease.join(",")})`
  )
  .join(", ")

const PANEL_VARIANTS = {
  enter: (direction: 1 | -1) => ({ x: direction > 0 ? "100%" : "-100%" }),
  center: { x: 0 },
  exit: (direction: 1 | -1) => ({ x: direction > 0 ? "-100%" : "100%" }),
} as const

type ButtonSkin = NonNullable<Parameters<typeof buttonVariants>[0]>

// ─── Root ─────────────────────────────────────────────────────────────────

/**
 * Trigger↔menu width sync, off unless the root asks for it.
 *
 * The trigger reports the width it lays out at on its own, which the menu takes
 * as a floor and the trigger then grows to meet. Freezing that natural width is
 * what keeps the two from chasing each other: were the floor read off the live
 * trigger (`--radix-dropdown-menu-trigger-width`), every pixel the trigger grew
 * would widen the menu, which would widen the trigger again.
 */
type MenuAlign = "start" | "center" | "end"

type TriggerWidthSync = {
  open: boolean
  /** The trigger's own width, measured while it carries no width of ours. */
  triggerWidth: number | null
  setTriggerWidth: (width: number | null) => void
  /** The width of the menu surface the trigger grows to. */
  menuWidth: number | null
  setMenuWidth: (width: number | null) => void
  /** The edge of the trigger the menu is aligned to, which has to stay put. */
  align: MenuAlign
  setAlign: (align: MenuAlign) => void
}

const TriggerWidthContext = React.createContext<TriggerWidthSync | null>(null)

/**
 * The root's open state, for the content.
 *
 * Radix puts the content's DOM through an open/close cycle, but the
 * `DropdownMenuContent` component that owns the panel path does not go with it:
 * the caller writes it into the tree, so it stays mounted in between. Reading
 * open state from here is what lets the panel path be reset for each opening.
 */
const DropdownMenuOpenContext = React.createContext(false)

/**
 * Pops one panel when a nested item is selected, or `null` when there is nowhere
 * to pop to — at the root panel, or under a content that asked to close on
 * select. Read by the item components, which use it to hold the selection open.
 */
const BackOnSelectContext = React.createContext<(() => void) | null>(null)

function DropdownMenu({
  expandTriggerToMenuWidth = false,
  open,
  defaultOpen,
  onOpenChange,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Root> & {
  /**
   * Widens the trigger to the width of the open menu, tweened, and never lets
   * the menu be narrower than the trigger — so the two read as one surface for
   * as long as the menu is open, and the trigger only ever grows.
   *
   * The trigger grows over what is beside it rather than pushing it along, and
   * the edge the menu is aligned to stays where it was, so neither the menu nor
   * the layout around the trigger moves while the tween runs.
   */
  expandTriggerToMenuWidth?: boolean
}) {
  const [openState, setOpenState] = React.useState(defaultOpen ?? false)
  const [triggerWidth, setTriggerWidth] = React.useState<number | null>(null)
  const [menuWidth, setMenuWidth] = React.useState<number | null>(null)
  const [align, setAlign] = React.useState<MenuAlign>("center")

  // The menu's own open state, mirrored rather than read from the content:
  // Radix keeps the content mounted for its close animation, so a trigger that
  // waited for the content to unmount would shrink a beat after the menu went.
  const isOpen = open ?? openState

  const sync = React.useMemo<TriggerWidthSync | null>(
    () =>
      expandTriggerToMenuWidth
        ? {
            open: isOpen,
            triggerWidth,
            setTriggerWidth,
            menuWidth,
            setMenuWidth,
            align,
            setAlign,
          }
        : null,
    [expandTriggerToMenuWidth, isOpen, triggerWidth, menuWidth, align]
  )

  return (
    <DropdownMenuOpenContext.Provider value={isOpen}>
      <TriggerWidthContext.Provider value={sync}>
        <DropdownMenuPrimitive.Root
          data-slot="dropdown-menu"
          open={open}
          defaultOpen={defaultOpen}
          onOpenChange={(next) => {
            setOpenState(next)
            onOpenChange?.(next)
          }}
          {...props}
        />
      </TriggerWidthContext.Provider>
    </DropdownMenuOpenContext.Provider>
  )
}

/** Assigns a node to our own state and to whatever ref the caller passed. */
function useNodeRef<T extends HTMLElement>(
  forwarded: React.Ref<T> | undefined
) {
  const [node, setNode] = React.useState<T | null>(null)

  const ref = React.useCallback(
    (element: T | null) => {
      setNode(element)
      if (typeof forwarded === "function") forwarded(element)
      else if (forwarded) forwarded.current = element
    },
    [forwarded]
  )

  return [node, ref] as const
}

function DropdownMenuPortal({
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Portal>) {
  return (
    <DropdownMenuPrimitive.Portal data-slot="dropdown-menu-portal" {...props} />
  )
}

function DropdownMenuGroup({
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Group>) {
  return (
    <DropdownMenuPrimitive.Group data-slot="dropdown-menu-group" {...props} />
  )
}

/**
 * Grows the trigger to the open menu's width and shrinks it back, for a root
 * that asked for it; a no-op otherwise, down to the measurement.
 *
 * The growth is taken straight back out in negative inline margins, split by the
 * edge the menu is aligned to — all of it on the far side for `align="start"`,
 * half on each for `"center"`. Two things fall out of that. The trigger's margin
 * box stays the width it started at, so nothing around it moves and its position
 * is still whatever the parent's layout made it. And the edge the menu hangs off
 * — the left one under `align="start"`, the centre under `"center"` — does not
 * move either, so the menu is not dragged sideways as the trigger widens under
 * it. The trigger grows over its neighbours rather than pushing them along.
 *
 * A trigger whose own content changes width while it is expanded (a selected
 * value it shows) cannot hold both of those: the compensation that fits the old
 * width is not the one that fits the new. The margins are therefore left to
 * tween from wherever they are on screen to the ones the new width needs, so the
 * trigger slides as it returns rather than snapping sideways before it starts.
 *
 * The menu's width is a floor under the trigger, not a width it shrinks back to:
 * a trigger whose own content is wider than the open menu — a long value it
 * shows, where crushing it to the menu's width would clip it and drag the
 * aligned edge with it — keeps its own width instead, and the menu's own floor
 * is raised to that width so the two still meet.
 *
 * The values are written to the element rather than rendered as style props: a
 * transition needs a concrete width to start from, and the trigger's own is
 * `auto`. The width start is written and flushed in the same layout pass as its
 * end, so the trigger never paints a frame at a width it is about to leave.
 * Width and the margins share one timing function.
 */
function useTriggerWidth(node: HTMLElement | null) {
  const sync = React.useContext(TriggerWidthContext)
  const prefersReducedMotion = useReducedMotion()
  const setTriggerWidth = sync?.setTriggerWidth
  // Read through a ref: `useReducedMotion` resolves after the first render, and
  // a re-run keyed to it would cut a tween that is already under way short.
  const reducedMotion = React.useRef(prefersReducedMotion)
  reducedMotion.current = prefersReducedMotion

  // The width the trigger would lay out at with no width of ours on it. A pinned
  // width hides it, so the pin is lifted for the length of one forced layout and
  // put straight back within the same pass — nothing paints at the unpinned
  // width. Read fresh rather than remembered: the trigger's own content can
  // change while it is pinned (the selected value it shows), and a remembered
  // width would send the tween to the width the trigger used to be.
  const readNatural = React.useCallback(() => {
    if (!node) return 0
    const width = node.style.width
    if (!width) return node.offsetWidth
    const transition = node.style.transition
    node.style.transition = "none"
    node.style.width = ""
    const measured = node.offsetWidth
    node.style.width = width
    node.style.transition = transition
    return measured
  }, [node])

  React.useLayoutEffect(() => {
    if (!node || !setTriggerWidth) return

    // The menu's floor is the width the trigger lays out at on its own. A width
    // of ours means the box is ours rather than the content's, so it is left out
    // of the floor; the effect below reads the real natural width when it needs
    // one.
    const measure = () => {
      if (node.style.width) return
      setTriggerWidth(node.offsetWidth)
    }

    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)

    return () => {
      observer.disconnect()
      setTriggerWidth(null)
    }
  }, [node, setTriggerWidth])

  // The menu's width while it is open, and nothing once it closes — a closed
  // menu still mounted for its exit animation must not hold the trigger open.
  const target = sync?.open ? sync.menuWidth : null
  const align = sync?.align ?? "center"

  // Bumped when the trigger's own rendered text changes while it is open; the
  // width effect keys to it. See the trailing effect.
  const [contentVersion, setContentVersion] = React.useState(0)
  const contentKey = React.useRef<string | null>(null)

  React.useLayoutEffect(() => {
    if (!node || !setTriggerWidth) return

    // Back to `auto`, so a trigger whose own content changes while it is closed
    // is not pinned to a width measured before it did.
    const release = () => {
      node.style.transition = ""
      node.style.width = ""
      node.style.marginInlineStart = ""
      node.style.marginInlineEnd = ""
    }

    // How much of the growth comes off the near side. The aligned edge takes
    // none of it, which is what keeps it where it was.
    const nearShare = align === "start" ? 0 : align === "end" ? 1 : 0.5

    const write = (width: number, natural: number) => {
      const growth = width - natural
      node.style.width = `${width}px`
      node.style.marginInlineStart = `${-growth * nearShare}px`
      node.style.marginInlineEnd = `${-growth * (1 - nearShare)}px`
    }

    // Nothing to move: closed, and already back at its own width.
    if (target == null && !node.style.width) return

    // An interrupted tween picks up from wherever it got to, which is what the
    // computed width reads while a tween owns the element.
    const from = node.style.width
      ? Math.round(parseFloat(getComputedStyle(node).width))
      : node.offsetWidth
    const natural = readNatural()
    // The menu's width is a floor and not a width to shrink back to. A trigger
    // whose own content is wider than the open menu — a long value it shows —
    // would otherwise be crushed down to the menu's width and clipped as the
    // surface follows a panel narrower than the trigger. The trigger only ever
    // grows.
    const to = target == null ? natural : Math.max(target, natural)

    // The menu's floor is the trigger's natural width, remembered from the last
    // time it was measured unpinned. Content that grew while the trigger was
    // pinned would leave that floor behind, and the menu would settle narrower
    // than the trigger it is meant to match; raise it to the fresh width so the
    // next pass brings the menu up to the trigger.
    if (target != null && natural > (sync?.triggerWidth ?? 0)) {
      setTriggerWidth(natural)
    }

    if (from === to || reducedMotion.current) {
      if (target == null) release()
      else {
        node.style.transition = ""
        write(to, natural)
      }
      return
    }

    // Only the width is pinned to a concrete start value, because its own is
    // `auto` and no transition can start from that. The margins are left where
    // they are, so they tween from the state on screen: when the content changed
    // while the trigger was expanded, the compensation on the element is based
    // on the width it used to have, and letting it flow into the one the new
    // width needs keeps the trigger from snapping sideways before the tween.
    node.style.transition = "none"
    node.style.width = `${from}px`
    void node.offsetWidth
    node.style.transition = TRIGGER_TRANSITION
    write(to, natural)

    // Only the way back is released: an expanded trigger has to keep the width
    // it was given, or it would snap to `auto` the moment the tween ended.
    if (target != null) return

    const settle = (event: TransitionEvent) => {
      if (event.target !== node || event.propertyName !== "width") return
      release()
    }

    node.addEventListener("transitionend", settle)
    return () => node.removeEventListener("transitionend", settle)
  }, [node, setTriggerWidth, target, align, readNatural, contentVersion])

  // The trigger's own content can change without the menu's width moving — a
  // longer selected value over short option labels — in which case the width
  // effect would never re-run and the wider content would be left overflowing
  // the pinned box. The rendered text is the only signal, so it is read back on
  // every render; a change while open and pinned bumps a version the width
  // effect keys to, which re-measures and grows the trigger atomically. Reading
  // text does not touch styles, so an expand tween under way is left alone.
  // Closed (or auto-width) content is already tracked by the resize observer,
  // so the ref is synced but no version is bumped there.
  React.useLayoutEffect(() => {
    if (!node || !setTriggerWidth) return
    const key = node.textContent
    const changed = contentKey.current !== null && key !== contentKey.current
    contentKey.current = key
    if (changed && sync?.open && node.style.width) {
      setContentVersion((version) => version + 1)
    }
  })
}

/**
 * Wears the `Button` skin so a bare trigger needs no wrapper, with a chevron
 * that flips while the menu is open.
 *
 * `asChild` is the shadcn escape hatch and stays one: the trigger then passes
 * straight through to Radix unstyled, so the long-standing
 * `<DropdownMenuTrigger asChild><Button/></DropdownMenuTrigger>` call site is
 * not double-styled.
 */
function DropdownMenuTrigger({
  className,
  variant = "outline",
  size = "default",
  asChild = false,
  children,
  ref: forwardedRef,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Trigger> & {
  variant?: ButtonSkin["variant"]
  size?: ButtonSkin["size"]
}) {
  const [node, ref] = useNodeRef<HTMLButtonElement>(forwardedRef)
  useTriggerWidth(node)

  if (asChild) {
    return (
      <DropdownMenuPrimitive.Trigger
        data-slot="dropdown-menu-trigger"
        asChild
        className={className}
        ref={ref}
        {...props}
      >
        {children}
      </DropdownMenuPrimitive.Trigger>
    )
  }

  return (
    <DropdownMenuPrimitive.Trigger
      data-slot="dropdown-menu-trigger"
      className={cn(
        "group/dropdown-trigger",
        buttonVariants({ variant, size, className })
      )}
      ref={ref}
      {...props}
    >
      {children}
      {/* An icon-sized trigger is already square around its one glyph; a second
          one would crowd it. */}
      {String(size).startsWith("icon") ? null : (
        <ChevronDownIcon className="opacity-60 transition-transform duration-200 group-data-[state=open]/dropdown-trigger:rotate-180 motion-reduce:transition-none" />
      )}
    </DropdownMenuPrimitive.Trigger>
  )
}

// ─── Sub-menu tree walking ────────────────────────────────────────────────

type SubElement = React.ReactElement<
  React.ComponentProps<typeof DropdownMenuSub>
>
type SubTriggerElement = React.ReactElement<
  React.ComponentProps<typeof DropdownMenuSubTrigger>
>
type SubContentElement = React.ReactElement<
  React.ComponentProps<typeof DropdownMenuSubContent>
>

/**
 * Walks `children` in document order — descending through fragments, arrays and
 * any element that just passes `children` along, such as a `DropdownMenuGroup`
 * — and swaps every `DropdownMenuSub` it can see for whatever `replace`
 * returns. It does not descend into a sub-menu: each call sees exactly one
 * panel's worth of items.
 *
 * The id handed to `replace` is the element's `key` when it has one and its
 * ordinal among the sub-menus of this panel otherwise, so it survives re-renders
 * and identifies the same sub-menu in both the resolve and the render pass.
 */
function mapSubs(
  children: React.ReactNode,
  replace: (sub: SubElement, id: string) => React.ReactNode
): React.ReactNode {
  let ordinal = 0

  const walk = (nodes: React.ReactNode): React.ReactNode =>
    React.Children.map(nodes, (child) => {
      if (!React.isValidElement(child)) return child

      if (child.type === DropdownMenuSub) {
        const id = child.key != null ? `key:${child.key}` : `pos:${ordinal}`
        ordinal += 1
        return replace(child as SubElement, id)
      }

      const inner = (child.props as { children?: React.ReactNode }).children
      if (inner == null || typeof inner === "function") return child

      return React.cloneElement(
        child as React.ReactElement<{ children?: React.ReactNode }>,
        undefined,
        walk(inner)
      )
    })

  return walk(children)
}

function splitSub(sub: SubElement) {
  let trigger: SubTriggerElement | null = null
  let content: SubContentElement | null = null

  for (const child of React.Children.toArray(sub.props.children)) {
    if (!React.isValidElement(child)) continue
    if (child.type === DropdownMenuSubTrigger) trigger = child as SubTriggerElement
    if (child.type === DropdownMenuSubContent) content = child as SubContentElement
  }

  return { trigger, content }
}

function findSub(children: React.ReactNode, id: string) {
  let found: SubElement | null = null

  mapSubs(children, (sub, subId) => {
    if (subId === id) found = sub
    return null
  })

  return found
}

// ─── Content ──────────────────────────────────────────────────────────────

/**
 * Where the back row sits relative to the panel's items. `"auto"` follows the
 * menu: the row goes on the edge nearest the trigger, so it stays close to where
 * the menu was opened from even when the menu flips above it.
 */
type BackTriggerPosition = "top" | "bottom" | "auto"

/** The concrete placement an `"auto"` position resolves to. */
type ResolvedBackTriggerPosition = Exclude<BackTriggerPosition, "auto">

/** What a caller-supplied back row is handed to draw and drive itself with. */
type BackTriggerRenderProps = {
  /** The label of the panel that would be returned to — a sub-trigger's content. */
  label: React.ReactNode
  /** Pops one panel. */
  back: () => void
  /** Where the row is being placed, so a custom row can match its margins. */
  position: ResolvedBackTriggerPosition
}

/**
 * The menu surface, and the whole of the panel machinery.
 *
 * A sub-menu does not fly out beside the menu on hover: clicking its trigger
 * replaces the panel in place, YouTube-style — the old panel slides out, the new
 * one slides in from the opposite side, and the surface tweens between the two
 * natural heights. A back row returns; so do `Escape` and `ArrowLeft`, which
 * pop one level instead of closing the menu. Selecting an item in a nested panel
 * returns too — there is a panel to go back to — while a root-panel selection
 * has nowhere to return and closes the menu as usual. Pass
 * `backOnSelect={false}` to close on every selection instead, `backTrigger` to
 * draw the back row yourself, and `backTriggerPosition` to pin it top or bottom
 * — by default it follows the side the menu opened on.
 *
 * The open panel is a path of sub-menu ids resolved against the live `children`
 * on every render (as in `SidebarNav`), so prop updates reach panels that are
 * already open and a path that no longer resolves falls back to its nearest
 * valid ancestor. Radix unmounts the content's DOM on close, but not this
 * component, which the caller keeps in the tree, so the path is reset as the
 * menu re-opens — before the new cycle's first commit, which is what keeps the
 * menu from both sliding out of the panel it was left on and swapping panels
 * behind the close animation.
 *
 * Resolving walks the element tree, so it only sees a `DropdownMenuSub` written
 * out here (in a group, a fragment or an `.map()` — all fine, though a sub-menu
 * coming out of an array wants a `key` like any other element: without one it is
 * identified by position, and reordering the array would pull an open panel over
 * to its sibling's content). One returned by a consumer's own component is
 * invisible to the walk and keeps Radix's native hover-opened side panel
 * instead of joining the stack.
 */
function DropdownMenuContent({
  className,
  panelClassName,
  sideOffset = 4,
  children,
  backOnSelect = true,
  backTrigger,
  backTriggerPosition = "auto",
  onEscapeKeyDown,
  onKeyDown,
  style,
  ref: forwardedRef,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Content> & {
  /** Classes for the sliding panel, which carries the menu's padding. */
  panelClassName?: string
  /**
   * Selecting an item in a nested panel pops back to the panel it came from
   * instead of closing the menu. The root panel has nowhere to go back to, so a
   * selection there always closes. Set `false` to close on every selection.
   */
  backOnSelect?: boolean
  /**
   * Replaces the built-in back row, which sits above the panel's items. It is
   * handed the destination's label and the `back` action, and whatever it
   * returns is rendered in the row's place; mark the returned row
   * `data-dropdown-menu-autofocus` to have it take focus when a panel opens.
   */
  backTrigger?: (props: BackTriggerRenderProps) => React.ReactNode
  /**
   * Where the back row sits: `"auto"` (the default) keeps it on the edge nearest
   * the trigger — above while the menu opens downwards, below once it flips
   * upwards — while `"top"` or `"bottom"` pins it. A custom `backTrigger` is told
   * the resolved position through its render props, so it can match its own
   * margins.
   */
  backTriggerPosition?: BackTriggerPosition
}) {
  const prefersReducedMotion = useReducedMotion()
  const menuOpen = React.useContext(DropdownMenuOpenContext)
  const [{ path, direction }, setPanel] = React.useState<{
    path: string[]
    direction: 1 | -1
  }>({ path: [], direction: 1 })

  // Back to the root panel for each opening, adjusted during render rather than
  // in an effect: the state has to be in place for the new cycle's first commit,
  // or `AnimatePresence` would play a slide out of the panel the menu was left
  // on, and an effect would first commit that stale panel and only then swap it.
  const [lastOpen, setLastOpen] = React.useState(menuOpen)
  if (menuOpen !== lastOpen) {
    setLastOpen(menuOpen)
    if (menuOpen) {
      setPanel((previous) =>
        previous.path.length === 0 && previous.direction === 1
          ? previous
          : { path: [], direction: 1 }
      )
    }
  }

  const trail: {
    id: string
    label: React.ReactNode
    className?: string
  }[] = []
  let panelChildren: React.ReactNode = children

  for (const id of path) {
    const sub = findSub(panelChildren, id)
    const parts = sub ? splitSub(sub) : null
    if (!parts?.content) break

    trail.push({
      id,
      label: parts.trigger?.props.children ?? null,
      className: parts.content.props.className,
    })
    panelChildren = parts.content.props.children
  }

  const current = trail.at(-1)
  // A sub-menu's own `className` follows it into the panel; its positioning
  // props (`sideOffset`, `alignOffset`, …) have nothing to place here.
  const currentClassName = current?.className
  const panelKey = ["root", ...trail.map((entry) => entry.id)].join("/")

  const open = (id: string) =>
    setPanel({ path: [...trail.map((entry) => entry.id), id], direction: 1 })
  const back = () =>
    setPanel({ path: trail.slice(0, -1).map((entry) => entry.id), direction: -1 })

  // There is only a selection to hold open where there is a panel to go back to,
  // which is the whole of the opt-out: a root-panel selection closes regardless.
  const goBackOnSelect = backOnSelect && trail.length > 0 ? back : null

  const items = mapSubs(panelChildren, (sub, id) => {
    const { trigger } = splitSub(sub)
    if (!trigger) return null

    return (
      <DropdownMenuPanelTrigger
        {...(trigger.props as PanelTriggerProps)}
        onOpen={() => open(id)}
      />
    )
  })

  // `"auto"` reads the side Radix settled on rather than the one it was asked
  // for: a menu pushed off the bottom of the viewport opens upwards, and the row
  // belongs on the edge left nearest the trigger. The side is tracked by the
  // effect below, which has the surface to read it from.
  const [side, setSide] = React.useState<string | null>(null)
  const backPosition: ResolvedBackTriggerPosition =
    backTriggerPosition === "auto"
      ? side === "top"
        ? "bottom"
        : "top"
      : backTriggerPosition

  // Held out of the selection-returns rule: a back row is the return, not another
  // selection to return from, and a custom one built from `DropdownMenuItem`
  // would otherwise pop twice.
  const backRow = current ? (
    <BackOnSelectContext.Provider value={null}>
      {backTrigger ? (
        backTrigger({
          label: current.label,
          back,
          position: backPosition,
        })
      ) : (
        <DropdownMenuBackTrigger
          label={current.label}
          onBack={back}
          position={backPosition}
        />
      )}
    </BackOnSelectContext.Provider>
  ) : null

  // ── Height ──
  // The clip sits on the box whose height is animated, so the menu's padding has
  // to live on the panel inside it — outside, the box would hug the items and
  // shave their focus rings.
  //
  // The box is never given a height it does not already have: the live panel is
  // in flow (`popLayout` lifts the outgoing one out), so the surface is the
  // right size in its very first layout, before any effect runs. That is what
  // Radix measures when it places the menu — height arriving a frame later is
  // what made a menu open downwards and then jump above the trigger once the
  // real size turned out not to fit. A navigation animates from the height the
  // box had to the incoming panel's, then hands the height back to the content.
  // A callback ref, not `useRef`: Radix mounts the content's DOM in a commit of
  // its own, after this component has already rendered once, and effects keyed
  // to a ref object would run against nothing and never run again.
  const [box, setBox] = React.useState<HTMLDivElement | null>(null)
  const previousHeight = React.useRef<number | null>(null)
  const settledPanel = React.useRef<string | null>(null)
  // Read through a ref: `useReducedMotion` resolves after the first render, and
  // an effect keyed to it would re-enter the block below for a panel that is
  // already in place — mid-tween, that would cut the tween short.
  const reducedMotion = React.useRef(prefersReducedMotion)
  reducedMotion.current = prefersReducedMotion

  // Tracks what the content settles at between navigations — the height the next
  // one tweens away from. `ResizeObserver` reports after layout, so when the
  // effect below runs on a navigation this still holds the height the user is
  // looking at rather than the one just committed. An inline height means a
  // tween owns the box, and its own values must not be recorded as the resting
  // one.
  React.useLayoutEffect(() => {
    if (!box) return

    previousHeight.current = box.offsetHeight

    const settle = (event: TransitionEvent) => {
      if (event.target !== box || event.propertyName !== "height") return
      // Back to `auto`, so content that grows after the slide — a row appearing
      // under a toggle — is not pinned to a height measured before it did.
      box.style.transition = ""
      box.style.height = ""
      previousHeight.current = box.offsetHeight
    }

    const observer = new ResizeObserver(() => {
      if (!box.style.height) previousHeight.current = box.offsetHeight
    })
    observer.observe(box)
    box.addEventListener("transitionend", settle)

    return () => {
      observer.disconnect()
      box.removeEventListener("transitionend", settle)
    }
  }, [box])

  React.useLayoutEffect(() => {
    if (!box) return

    let incoming: HTMLElement | null = null

    // The outgoing panel stays mounted for the slide, where it would otherwise
    // keep answering clicks, arrow keys and typeahead from behind the new one.
    for (const panel of box.querySelectorAll<HTMLElement>(
      '[data-slot="dropdown-menu-panel"]'
    )) {
      if (panel.dataset.panelKey === panelKey) {
        incoming = panel
        panel.toggleAttribute("inert", false)
        panel.removeAttribute("aria-hidden")
        panel.style.pointerEvents = ""
        // A panel can come back before it has finished leaving — back out of a
        // sub-menu fast enough and this is the same element, still carrying the
        // styles below.
        panel.style.position = ""
        panel.style.inset = ""
      } else {
        panel.toggleAttribute("inert", true)
        panel.setAttribute("aria-hidden", "true")
        panel.style.pointerEvents = "none"
        // Out of flow for the crossing, or the box would be as tall as both
        // panels stacked instead of the one arriving. `AnimatePresence`'s
        // `popLayout` is the obvious way to get this and does not work here: it
        // measures the child it is handed, and the child is `DropdownMenuPanel`,
        // which passes no ref down to the element that actually lays out.
        panel.style.position = "absolute"
        panel.style.inset = "0 0 auto 0"
      }
    }

    // The menu opens at its natural size, so the first panel has nothing to
    // animate from and Radix has already focused it. A re-run for any other
    // reason has nowhere to move to either.
    const previousPanel = settledPanel.current
    settledPanel.current = panelKey
    if (previousPanel == null || previousPanel === panelKey) return

    // Radix focused the menu on open; from then on the focused row unmounts with
    // its panel, so focus has to be handed to the incoming one explicitly or the
    // menu loses its roving focus altogether.
    const focusTarget =
      incoming?.querySelector<HTMLElement>("[data-dropdown-menu-autofocus]") ??
      incoming?.querySelector<HTMLElement>(
        '[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"]'
      )

    focusTarget?.focus()

    if (!incoming) return

    // Measured off the panel, not the box: the outgoing one is still mounted for
    // the slide. An interrupted navigation picks up from wherever the box got
    // to, which is what the computed height reads while a tween owns it.
    const to = incoming.offsetHeight
    const from = box.style.height
      ? Math.round(parseFloat(getComputedStyle(box).height))
      : previousHeight.current

    previousHeight.current = to

    if (from == null || from === to || reducedMotion.current) {
      box.style.transition = ""
      box.style.height = ""
      return
    }

    // A CSS transition rather than a scripted one: the start height is written
    // and flushed in this same layout pass, so the box never paints a frame at
    // the height it is about to leave.
    box.style.transition = "none"
    box.style.height = `${from}px`
    void box.offsetHeight
    box.style.transition = HEIGHT_TRANSITION
    box.style.height = `${to}px`
  }, [box, panelKey])

  // ── Width ──
  // Only for a root that asked the trigger to follow the menu. The surface is
  // measured rather than computed: it sizes itself to its widest row, within the
  // min and max the classes below set, and a navigation can change it.
  const widthSync = React.useContext(TriggerWidthContext)
  const setMenuWidth = widthSync?.setMenuWidth
  const setAlign = widthSync?.setAlign
  const [surface, surfaceRef] = useNodeRef<HTMLDivElement>(forwardedRef)

  React.useLayoutEffect(() => {
    if (!surface || !setMenuWidth || !setAlign) return

    const measure = () => {
      setMenuWidth(surface.offsetWidth)
      // Read back rather than taken from the prop: a menu that ran out of room
      // is aligned to the edge Radix settled on, not the one it was asked for.
      const settled = surface.dataset.align
      if (settled === "start" || settled === "center" || settled === "end") {
        setAlign(settled)
      }
    }

    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(surface)

    return () => {
      observer.disconnect()
      setMenuWidth(null)
    }
  }, [surface, setMenuWidth, setAlign])

  // ── Back row side ──
  // Only for an `"auto"` back row. Radix writes the resolved side to
  // `data-side`, and it can change after the first placement — the menu flips
  // when it runs out of room, and follows the trigger on scroll — so the
  // attribute is watched rather than read once.
  React.useLayoutEffect(() => {
    if (!surface || backTriggerPosition !== "auto") return

    const read = () => setSide(surface.dataset.side ?? null)
    read()
    const observer = new MutationObserver(read)
    observer.observe(surface, {
      attributes: true,
      attributeFilter: ["data-side"],
    })

    return () => observer.disconnect()
  }, [surface, backTriggerPosition])

  return (
    <BackOnSelectContext.Provider value={goBackOnSelect}>
      <DropdownMenuPrimitive.Portal>
        <DropdownMenuPrimitive.Content
          data-slot="dropdown-menu-content"
          sideOffset={sideOffset}
          ref={surfaceRef}
          style={
            widthSync?.triggerWidth
              ? ({
                  ...style,
                  "--panit-menu-trigger-w": `${widthSync.triggerWidth}px`,
                } as React.CSSProperties)
              : style
          }
          className={cn(
            // `--panit-menu-frame` is the surface's own border, which the panel's
            // scroll cap has to leave out of the height Radix budgets for the
            // whole menu. Override it alongside a heavier `border-*`.
            // The menu is never narrower than the trigger it is placed against
            // when the trigger is following it; `--panit-menu-trigger-w` is unset
            // otherwise, and the floor is the plain one.
            "z-50 [--panit-menu-frame:2px] min-w-[max(12rem,var(--panit-menu-trigger-w,0px))] max-w-[min(24rem,var(--radix-dropdown-menu-content-available-width,24rem))] origin-(--radix-dropdown-menu-content-transform-origin) rounded-lg border border-border bg-popover text-popover-foreground shadow-lg",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95",
            "data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2",
            className
          )}
          onEscapeKeyDown={(event) => {
            onEscapeKeyDown?.(event)
            if (event.defaultPrevented) return
            if (trail.length === 0) return
            // Escape backs out of the sub-menu before it closes the menu.
            event.preventDefault()
            back()
          }}
          onKeyDown={(event) => {
            onKeyDown?.(event)
            if (event.defaultPrevented) return
            if (event.key === "ArrowLeft" && trail.length > 0) {
              event.preventDefault()
              back()
            }
          }}
          {...props}
        >
          <div
            ref={setBox}
            // `clip`, not `hidden`: focusing a row on the outgoing panel would
            // otherwise scroll the box sideways and leave the panel offset.
            style={{ overflow: "clip" }}
          >
            {/* `relative` is what the outgoing panel is pinned against. */}
            <div className="relative">
              <AnimatePresence custom={direction} initial={false}>
                <DropdownMenuPanel
                  key={panelKey}
                  panelKey={panelKey}
                  direction={direction}
                  reduceMotion={Boolean(prefersReducedMotion)}
                  className={cn(panelClassName, currentClassName)}
                >
                  {backPosition === "top" && backRow}
                  {items}
                  {backPosition !== "top" && backRow}
                </DropdownMenuPanel>
              </AnimatePresence>
            </div>
          </div>
        </DropdownMenuPrimitive.Content>
      </DropdownMenuPrimitive.Portal>
    </BackOnSelectContext.Provider>
  )
}

/**
 * The back row a panel encloses its items with, unless the content replaced it
 * with `backTrigger`. Drawing it as a menu item — and marking it for autofocus —
 * is what lets a panel hand focus back to the row as it arrives.
 */
function DropdownMenuBackTrigger({
  label,
  onBack,
  position,
}: {
  label: React.ReactNode
  onBack: () => void
  position: ResolvedBackTriggerPosition
}) {
  return (
    <DropdownMenuPrimitive.Item
      data-slot="dropdown-menu-back"
      data-dropdown-menu-autofocus=""
      onSelect={(event) => event.preventDefault()}
      onClick={onBack}
      className={cn(
        "flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm font-medium text-muted-foreground outline-hidden transition-colors select-none focus:bg-accent focus:text-accent-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
        position === "top" ? "mb-1" : "mt-1"
      )}
    >
      <ArrowLeftIcon />
      <span className="flex flex-1 items-center gap-2 truncate">{label}</span>
    </DropdownMenuPrimitive.Item>
  )
}

function DropdownMenuPanel({
  panelKey,
  direction,
  reduceMotion,
  className,
  children,
}: {
  panelKey: string
  direction: 1 | -1
  reduceMotion: boolean
  className?: string
  children: React.ReactNode
}) {
  return (
    <motion.div
      data-slot="dropdown-menu-panel"
      data-panel-key={panelKey}
      custom={direction}
      variants={reduceMotion ? undefined : PANEL_VARIANTS}
      initial={reduceMotion ? false : "enter"}
      animate={reduceMotion ? undefined : "center"}
      exit={reduceMotion ? undefined : "exit"}
      transition={PANEL_TRANSITION}
      // The live panel stays in flow and is what gives the surface its height;
      // the outgoing one is lifted out of flow for the crossing.
      className={cn(
        "w-full max-h-[calc(var(--radix-dropdown-menu-content-available-height)-var(--panit-menu-frame,0px))] overflow-y-auto p-1",
        className
      )}
    >
      {children}
    </motion.div>
  )
}

// ─── Items ────────────────────────────────────────────────────────────────

const itemClassName = cn(
  "relative flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm outline-hidden transition-colors select-none",
  "focus:bg-accent focus:text-accent-foreground",
  "data-[variant=destructive]:text-destructive data-[variant=destructive]:focus:bg-destructive/10 data-[variant=destructive]:focus:text-destructive data-[variant=destructive]:*:[svg]:!text-destructive",
  "data-disabled:pointer-events-none data-disabled:opacity-50",
  "data-[inset]:pl-8",
  "[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground"
)

/**
 * Runs the caller's `onSelect`, then — where a nested selection should return
 * rather than close — stops the menu's own close and pops one panel instead. A
 * caller that has already called `preventDefault` keeps its own outcome.
 */
function useItemSelect(onSelect: ((event: Event) => void) | undefined) {
  const goBack = React.useContext(BackOnSelectContext)

  return (event: Event) => {
    onSelect?.(event)
    if (event.defaultPrevented || !goBack) return
    event.preventDefault()
    goBack()
  }
}

function DropdownMenuItem({
  className,
  inset,
  variant = "default",
  onSelect,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Item> & {
  inset?: boolean
  variant?: "default" | "destructive"
}) {
  const handleSelect = useItemSelect(onSelect)

  return (
    <DropdownMenuPrimitive.Item
      data-slot="dropdown-menu-item"
      data-inset={inset ? "" : undefined}
      data-variant={variant}
      className={cn(itemClassName, className)}
      onSelect={handleSelect}
      {...props}
    />
  )
}

function DropdownMenuCheckboxItem({
  className,
  children,
  checked,
  onSelect,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.CheckboxItem>) {
  const handleSelect = useItemSelect(onSelect)

  return (
    <DropdownMenuPrimitive.CheckboxItem
      data-slot="dropdown-menu-checkbox-item"
      className={cn(itemClassName, "pr-2 pl-8", className)}
      checked={checked}
      onSelect={handleSelect}
      {...props}
    >
      <span className="pointer-events-none absolute left-2 flex size-3.5 items-center justify-center">
        <DropdownMenuPrimitive.ItemIndicator>
          <CheckIcon className="size-4 animate-in zoom-in-50 duration-150 motion-reduce:animate-none" />
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
      {children}
    </DropdownMenuPrimitive.CheckboxItem>
  )
}

function DropdownMenuRadioGroup({
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.RadioGroup>) {
  return (
    <DropdownMenuPrimitive.RadioGroup
      data-slot="dropdown-menu-radio-group"
      {...props}
    />
  )
}

function DropdownMenuRadioItem({
  className,
  children,
  onSelect,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.RadioItem>) {
  const handleSelect = useItemSelect(onSelect)

  return (
    <DropdownMenuPrimitive.RadioItem
      data-slot="dropdown-menu-radio-item"
      className={cn(itemClassName, "pr-2 pl-8", className)}
      onSelect={handleSelect}
      {...props}
    >
      <span className="pointer-events-none absolute left-2 flex size-3.5 items-center justify-center">
        <DropdownMenuPrimitive.ItemIndicator>
          <CircleIcon className="size-2 fill-current animate-in zoom-in-50 duration-150 motion-reduce:animate-none" />
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
      {children}
    </DropdownMenuPrimitive.RadioItem>
  )
}

function DropdownMenuLabel({
  className,
  inset,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Label> & {
  inset?: boolean
}) {
  return (
    <DropdownMenuPrimitive.Label
      data-slot="dropdown-menu-label"
      data-inset={inset ? "" : undefined}
      className={cn(
        "px-2 py-1.5 text-xs font-medium text-foreground/80 data-[inset]:pl-8",
        className
      )}
      {...props}
    />
  )
}

function DropdownMenuSeparator({
  className,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Separator>) {
  return (
    <DropdownMenuPrimitive.Separator
      data-slot="dropdown-menu-separator"
      className={cn("pointer-events-none -mx-1 my-1 h-px bg-border", className)}
      {...props}
    />
  )
}

function DropdownMenuShortcut({
  className,
  ...props
}: React.ComponentProps<"span">) {
  return (
    <span
      data-slot="dropdown-menu-shortcut"
      className={cn(
        "ml-auto text-xs tracking-widest text-muted-foreground",
        className
      )}
      {...props}
    />
  )
}

// ─── Sub-menus ────────────────────────────────────────────────────────────

/**
 * The row a sub-menu is entered by. It is a plain item, not a Radix sub-trigger:
 * the panel replaces itself in place, so selecting it must not close the menu.
 */
type PanelTriggerProps = Omit<
  React.ComponentProps<typeof DropdownMenuSubTrigger>,
  "onSelect"
> & { onOpen: () => void }

function DropdownMenuPanelTrigger({
  className,
  inset,
  children,
  onOpen,
  ...props
}: PanelTriggerProps) {
  return (
    <DropdownMenuPrimitive.Item
      {...props}
      data-slot="dropdown-menu-sub-trigger"
      data-inset={inset ? "" : undefined}
      className={cn(itemClassName, className)}
      // Entering a sub-menu replaces this panel; it must not close the menu.
      onSelect={(event) => {
        event.preventDefault()
        onOpen()
      }}
    >
      <span className="flex flex-1 items-center gap-2 truncate">{children}</span>
      <ChevronRightIcon className="ml-auto opacity-60" />
    </DropdownMenuPrimitive.Item>
  )
}

/**
 * Declares a sub-menu. `DropdownMenuContent` normally intercepts it and turns it
 * into a panel in the stack; the Radix implementation below is the fallback for
 * a sub-menu the content's tree walk cannot see, which keeps rendering as the
 * shadcn hover-opened side panel.
 */
function DropdownMenuSub({
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.Sub>) {
  return <DropdownMenuPrimitive.Sub data-slot="dropdown-menu-sub" {...props} />
}

function DropdownMenuSubTrigger({
  className,
  inset,
  children,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.SubTrigger> & {
  inset?: boolean
}) {
  return (
    <DropdownMenuPrimitive.SubTrigger
      data-slot="dropdown-menu-sub-trigger"
      data-inset={inset ? "" : undefined}
      className={cn(
        itemClassName,
        "data-[state=open]:bg-accent data-[state=open]:text-accent-foreground",
        className
      )}
      {...props}
    >
      <span className="flex flex-1 items-center gap-2 truncate">{children}</span>
      <ChevronRightIcon className="ml-auto opacity-60" />
    </DropdownMenuPrimitive.SubTrigger>
  )
}

function DropdownMenuSubContent({
  className,
  ...props
}: React.ComponentProps<typeof DropdownMenuPrimitive.SubContent>) {
  return (
    <DropdownMenuPrimitive.SubContent
      data-slot="dropdown-menu-sub-content"
      className={cn(
        "z-50 min-w-[8rem] origin-(--radix-dropdown-menu-content-transform-origin) overflow-hidden rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-lg",
        "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95",
        className
      )}
      {...props}
    />
  )
}

export {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuPortal,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
}
