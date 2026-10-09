"use client"

import { useId, useState, type ComponentType, type ReactNode } from "react"
import { ArrowLeft, ChevronDown, ChevronRight, Plus } from "lucide-react"
import { AnimatePresence, LayoutGroup, motion, useReducedMotion } from "framer-motion"

import { cn } from "@/lib/utils"

/*
 * Layout grid. The panel carries an 8px inset and every row another 8px, so
 * there are exactly two left edges: 8px for surfaces (row fills, the action
 * row) and 16px for content (icons, labels, section headings). Nothing else
 * adds horizontal padding.
 */
const ROW =
  "relative flex h-8 w-full items-center gap-2.5 rounded-md px-2 text-left text-[13px] leading-none outline-none transition-colors " +
  "focus-visible:ring-2 focus-visible:ring-sidebar-ring/60"
const ROW_IDLE =
  "text-sidebar-foreground/75 hover:bg-sidebar-foreground/[0.035] hover:text-sidebar-foreground"
const ROW_ACTIVE = "text-sidebar-foreground"
const ICON = "relative size-4 shrink-0 transition-colors"
const ICON_IDLE = "text-muted-foreground group-hover/row:text-sidebar-foreground/80"
const HEADING = "flex h-7 items-center px-2 text-[11.5px] font-medium text-muted-foreground"

/** Fill strength for the active row; the actions overlay blends into it. */
const ACTIVE_MIX = "color-mix(in oklab, var(--sidebar), var(--sidebar-foreground) 8.5%)"
const HOVER_MIX = "color-mix(in oklab, var(--sidebar), var(--sidebar-foreground) 3.5%)"

const FILL_TRANSITION = { type: "spring", stiffness: 520, damping: 42, mass: 0.6 } as const

const PANEL_TRANSITION = {
  duration: 0.22,
  ease: [0.32, 0.72, 0, 1],
} as const

const PANEL_VARIANTS = {
  enter: (direction: 1 | -1) => ({ x: direction > 0 ? "100%" : "-100%" }),
  center: { x: 0 },
  exit: (direction: 1 | -1) => ({ x: direction > 0 ? "-100%" : "100%" }),
} as const

type NavSubmenu = {
  /** Heading shown above the sub-menu's own sections. */
  label?: string
  /** Label of the back button. Defaults to `"Back"`. */
  backLabel?: string
  /** Rendered between the back button and the sections, for context cards. */
  header?: ReactNode
  /**
   * Called when the back button is pressed, before the panel pops. Lets a
   * consumer whose panels correspond to routes navigate out in step with the
   * panel, instead of the nav returning to root while the page stays put.
   */
  onBack?: () => void
  sections: NavSection[]
}

type NavItem = {
  label: string
  /** Omit for list-like rows such as recent threads, where titles need the width. */
  icon?: ComponentType<{ className?: string }>
  /**
   * Marks the current page. An item whose sub-menu, or collapsed children,
   * contain the current page is shown as active too, so the selection stays
   * visible from where the user is looking.
   */
  active?: boolean
  badge?: ReactNode
  /**
   * Controls revealed over the end of the row on hover or focus, such as a
   * "more" button. They overlay the label instead of reserving its width.
   */
  actions?: ReactNode
  /** `"muted"` for secondary links such as "View all". */
  tone?: "default" | "muted"
  /** Called on click, including when the item also opens a sub-menu or its children. */
  onClick?: () => void
  /** Slides a nested panel in from the right when the item is clicked. Nests to any depth. */
  submenu?: NavSubmenu
  /**
   * Rows shown inline under this one, indented to its label, such as a
   * project's conversations. One level only: children's own `children` are
   * ignored; use `submenu` for anything deeper.
   *
   * Clicking the row runs `onClick` and expands it; the chevron that replaces
   * the icon on hover toggles it without navigating.
   */
  children?: NavItem[]
  /** Uncontrolled initial state for `children`. Defaults to collapsed. */
  defaultOpen?: boolean
  /** Controlled state for `children`, with `onOpenChange`. */
  open?: boolean
  onOpenChange?: (open: boolean) => void
}

type NavSection = {
  label?: string
  /** Renders a divider above the section, for grouping unlabeled sections. */
  separator?: boolean
  /** Makes the heading a toggle that hides the section's items. Needs `label`. */
  collapsible?: boolean
  /** Uncontrolled initial state for a `collapsible` section. Defaults to open. */
  defaultOpen?: boolean
  items: NavItem[]
}

function itemContainsActive(item: NavItem): boolean {
  return Boolean(
    item.active ||
      item.children?.some((child) => child.active) ||
      (item.submenu && containsActive(item.submenu.sections))
  )
}

function containsActive(sections: NavSection[]): boolean {
  return sections.some((section) => section.items.some(itemContainsActive))
}

/** Uncontrolled state unless `value` is given, in which case `onChange` owns it. */
function useDisclosure(value: boolean | undefined, initial: boolean, onChange?: (open: boolean) => void) {
  const [own, setOwn] = useState(initial)
  const open = value ?? own
  return [
    open,
    (next: boolean) => {
      if (value === undefined) setOwn(next)
      onChange?.(next)
    },
  ] as const
}

const COLLAPSE = {
  initial: { height: 0, opacity: 0 },
  animate: { height: "auto", opacity: 1 },
  exit: { height: 0, opacity: 0 },
  transition: { duration: 0.18, ease: [0.32, 0.72, 0, 1] },
} as const

export function SidebarNav({
  sections,
  newLabel,
  onNewClick,
  ariaLabel,
  className,
  panelClassName,
}: {
  sections: NavSection[]
  /** Adds an action row pinned to the bottom, such as "New project". */
  newLabel?: string
  onNewClick?: () => void
  ariaLabel?: string
  className?: string
  /**
   * Classes for the sliding panel, which carries the nav's padding.
   * `className` is applied to the outer nav and cannot reach the panel.
   */
  panelClassName?: string
}) {
  const uid = useId()
  const prefersReducedMotion = useReducedMotion()
  const [{ path, direction }, setPanel] = useState<{ path: string[]; direction: 1 | -1 }>({
    path: [],
    direction: 1,
  })

  // The open panel is a path of item labels resolved against the live `sections`
  // prop on every render, so prop updates reach panels that are already open. A
  // path that no longer resolves falls back to its nearest valid ancestor.
  const trail: { label: string; submenu: NavSubmenu }[] = []
  let panelSections = sections

  for (const label of path) {
    const item = panelSections.flatMap((section) => section.items).find((it) => it.label === label)

    if (!item?.submenu) break

    trail.push({ label, submenu: item.submenu })
    panelSections = item.submenu.sections
  }

  const current = trail.at(-1)
  const panelKey = ["root", ...trail.map((entry) => entry.label)].join("/")

  function openSubmenu(label: string) {
    setPanel({ path: [...trail.map((entry) => entry.label), label], direction: 1 })
  }

  function goBack() {
    current?.submenu.onBack?.()
    setPanel({ path: trail.slice(0, -1).map((entry) => entry.label), direction: -1 })
  }

  return (
    <nav
      aria-label={ariaLabel}
      className={cn("flex h-full flex-col bg-sidebar text-sidebar-foreground", className)}
    >
      {/* `overflow-clip`, not `overflow-hidden`: focusing a button on the outgoing
          panel would otherwise scroll it sideways and leave the panel offset. */}
      <div className="relative min-h-0 flex-1 overflow-clip">
        <AnimatePresence custom={direction} initial={false} mode="popLayout">
          <motion.div
            key={panelKey}
            custom={direction}
            variants={prefersReducedMotion ? undefined : PANEL_VARIANTS}
            initial={prefersReducedMotion ? false : "enter"}
            animate={prefersReducedMotion ? undefined : "center"}
            exit={prefersReducedMotion ? undefined : "exit"}
            transition={PANEL_TRANSITION}
            className={cn("absolute inset-0 flex flex-col gap-3 overflow-y-auto p-2", panelClassName)}
          >
            {current ? (
              <div className="flex flex-col gap-1">
                <button type="button" onClick={goBack} className={cn("group/row", ROW, ROW_IDLE)}>
                  <ArrowLeft className={cn(ICON, ICON_IDLE)} />
                  <span className="truncate">{current.submenu.backLabel ?? "Back"}</span>
                </button>
                {current.submenu.header}
                {current.submenu.label ? <div className={HEADING}>{current.submenu.label}</div> : null}
              </div>
            ) : null}
            <LayoutGroup id={`${uid}-${panelKey}`}>
              <NavSections onOpenSubmenu={openSubmenu} sections={panelSections} />
            </LayoutGroup>
          </motion.div>
        </AnimatePresence>
      </div>

      {newLabel ? (
        <div className={cn("p-2 pt-0", panelClassName)}>
          <button type="button" onClick={onNewClick} className={cn("group/row", ROW, ROW_IDLE)}>
            <Plus className={cn(ICON, ICON_IDLE)} />
            <span className="truncate">{newLabel}</span>
          </button>
        </div>
      ) : null}
    </nav>
  )
}

function NavSections({
  onOpenSubmenu,
  sections,
}: {
  onOpenSubmenu: (label: string) => void
  sections: NavSection[]
}) {
  return (
    <>
      {sections.map((section, index) => (
        <NavSectionBlock
          key={section.label ?? section.items.map((item) => item.label).join("-")}
          section={section}
          first={index === 0}
          onOpenSubmenu={onOpenSubmenu}
        />
      ))}
    </>
  )
}

function NavSectionBlock({
  section,
  first,
  onOpenSubmenu,
}: {
  section: NavSection
  first: boolean
  onOpenSubmenu: (label: string) => void
}) {
  const reduced = useReducedMotion()
  const collapsible = Boolean(section.collapsible && section.label)
  const [open, setOpen] = useDisclosure(undefined, section.defaultOpen ?? true)

  const items = (
    <div className="space-y-px">
      {section.items.map((item) => (
        <NavRow key={item.label} item={item} onOpenSubmenu={onOpenSubmenu} />
      ))}
    </div>
  )

  return (
    <div className={cn(section.separator && !first && "border-t border-sidebar-border/70 pt-3")}>
      {section.label ? (
        collapsible ? (
          <button
            type="button"
            onClick={() => setOpen(!open)}
            aria-expanded={open}
            className={cn(
              HEADING,
              "group/heading gap-1 rounded-md outline-none hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring/60"
            )}
          >
            {section.label}
            <ChevronDown
              className={cn("size-3.5 transition-transform", !open && "-rotate-90")}
              aria-hidden
            />
          </button>
        ) : (
          <div className={HEADING}>{section.label}</div>
        )
      ) : null}
      {collapsible ? (
        <AnimatePresence initial={false}>
          {open ? (
            <motion.div {...(reduced ? {} : COLLAPSE)} className="overflow-hidden">
              {items}
            </motion.div>
          ) : null}
        </AnimatePresence>
      ) : (
        items
      )}
    </div>
  )
}

function NavRow({
  item,
  onOpenSubmenu,
  nested = false,
}: {
  item: NavItem
  onOpenSubmenu: (label: string) => void
  nested?: boolean
}) {
  const reduced = useReducedMotion()
  const children = nested ? undefined : item.children
  const [open, setOpen] = useDisclosure(item.open, item.defaultOpen ?? false, item.onOpenChange)
  const Icon = item.icon
  // While its children are showing, the child carries the fill, not the parent.
  const active = open && children ? Boolean(item.active) : itemContainsActive(item)
  const muted = item.tone === "muted"

  return (
    <>
      <div
        className={cn(
          "group/row",
          ROW,
          "p-0",
          active ? ROW_ACTIVE : muted ? "text-muted-foreground hover:bg-sidebar-foreground/[0.035] hover:text-sidebar-foreground" : ROW_IDLE
        )}
      >
        {/* The one moving indicator: a fill that travels between rows. Nothing
            else about the row changes on select, so labels never shift. */}
        {active ? (
          <motion.span
            layoutId="active-fill"
            aria-hidden
            transition={reduced ? { duration: 0 } : FILL_TRANSITION}
            className="absolute inset-0 rounded-md bg-sidebar-foreground/[0.085]"
          />
        ) : null}
        <button
          type="button"
          onClick={() => {
            item.onClick?.()
            if (item.submenu) onOpenSubmenu(item.label)
            if (children && !open) setOpen(true)
          }}
          aria-current={item.active ? "page" : undefined}
          aria-haspopup={item.submenu ? "menu" : undefined}
          className="relative flex h-full min-w-0 flex-1 items-center gap-2.5 rounded-md px-2 text-left outline-none"
        >
          {Icon ? (
            <Icon
              className={cn(
                ICON,
                active ? "text-sidebar-foreground" : ICON_IDLE,
                children && "group-hover/row:opacity-0 group-has-[:focus-visible]/row:opacity-0"
              )}
            />
          ) : null}
          <span className="min-w-0 flex-1 truncate">{item.label}</span>
          {item.badge}
          {item.submenu ? (
            <ChevronRight className="relative size-3.5 shrink-0 text-muted-foreground/70" />
          ) : null}
        </button>

        {/* The chevron sits over the icon: toggles without navigating. */}
        {children && Icon ? (
          <button
            type="button"
            onClick={() => setOpen(!open)}
            aria-expanded={open}
            aria-label={`${open ? "Collapse" : "Expand"} ${item.label}`}
            className={cn(
              "absolute inset-y-0 left-0 grid w-8 place-items-center rounded-md text-muted-foreground outline-none",
              "opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-sidebar-ring/60",
              "hover:text-sidebar-foreground"
            )}
          >
            <ChevronRight className={cn("size-4 transition-transform", open && "rotate-90")} />
          </button>
        ) : null}

        {item.actions ? (
          <div
            className={cn(
              "absolute inset-y-0 right-0 flex items-center gap-0.5 rounded-r-md pr-1 pl-6",
              "opacity-0 transition-opacity group-hover/row:opacity-100 focus-within:opacity-100 has-[[data-state=open]]:opacity-100",
              "[&_button]:grid [&_button]:size-6 [&_button]:place-items-center [&_button]:rounded [&_button]:text-muted-foreground",
              "[&_button:hover]:bg-sidebar-foreground/10 [&_button:hover]:text-sidebar-foreground"
            )}
            // Fades the label out under the actions; the stop matches the fill below.
            style={{
              background: `linear-gradient(to left, ${active ? ACTIVE_MIX : HOVER_MIX} 65%, transparent)`,
            }}
          >
            {item.actions}
          </div>
        ) : null}
      </div>

      {children ? (
        <AnimatePresence initial={false}>
          {open ? (
            <motion.div {...(reduced ? {} : COLLAPSE)} className="overflow-hidden">
              {/* Children start at the parent's label edge; the guide runs
                  under the parent's icon. Without an icon they indent by 12px. */}
              <div className={cn("relative space-y-px pt-px", Icon ? "pl-[26px]" : "pl-3")}>
                <span
                  aria-hidden
                  className={cn(
                    "absolute inset-y-0 w-px bg-sidebar-border",
                    Icon ? "left-4" : "left-1.5"
                  )}
                />
                {children.map((child) => (
                  <NavRow key={child.label} item={child} onOpenSubmenu={onOpenSubmenu} nested />
                ))}
              </div>
            </motion.div>
          ) : null}
        </AnimatePresence>
      ) : null}
    </>
  )
}
