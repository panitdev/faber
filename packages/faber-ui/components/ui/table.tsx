import * as React from "react"
import { ArrowDown, ArrowUp, ChevronsUpDown } from "lucide-react"

import { cn } from "@/lib/utils"

// ─── Table ────────────────────────────────────────────────────────────────
//
// Drop-in for the shadcn table: same parts, same `data-slot`s, same
// `data-state="selected"` row convention. Everything added here is optional.
//
// Styling is driven from the <table> element through `group/table` and data
// attributes (`data-variant`, `data-density`, `data-sticky-header`), so the
// parts stay plain shadcn-shaped components with no required context.
//
// The table uses `border-separate` with borders on the cells instead of on the
// rows. With `border-collapse` a sticky header loses its bottom border as soon
// as it sticks; cell borders stay attached.

type TableVariant = "framed" | "plain"
type TableDensity = "compact" | "default" | "comfortable"

interface TableProps extends React.ComponentProps<"table"> {
  /**
   * `framed` (default) sits in one rounded, bordered card surface with a
   * tinted header. `plain` drops the frame for tables that already live
   * inside a card or a page section.
   */
  variant?: TableVariant
  /** Row height: compact 36px, default 44px, comfortable 56px. */
  density?: TableDensity
  /**
   * Pin the header while the body scrolls. Give the container a height via
   * `containerClassName` (e.g. `max-h-96`) so there is something to scroll.
   */
  stickyHeader?: boolean
  /** Classes for the scroll container that wraps the <table>. */
  containerClassName?: string
}

function Table({
  variant = "framed",
  density = "default",
  stickyHeader = false,
  containerClassName,
  className,
  ...props
}: TableProps) {
  return (
    <div
      data-slot="table-container"
      data-variant={variant}
      className={cn(
        "relative w-full overflow-auto",
        variant === "framed" && "rounded-xl border border-border/60 bg-card",
        containerClassName,
      )}
    >
      <table
        data-slot="table"
        data-variant={variant}
        data-density={density}
        data-sticky-header={stickyHeader || undefined}
        className={cn(
          "group/table w-full caption-bottom border-separate border-spacing-0 text-sm tabular-nums",
          className,
        )}
        {...props}
      />
    </div>
  )
}

// ─── Sections ─────────────────────────────────────────────────────────────

function TableHeader({ className, ...props }: React.ComponentProps<"thead">) {
  return (
    <thead
      data-slot="table-header"
      className={cn(
        // Solid (not translucent) fill so a sticky header hides rows under it.
        // Painted on the row, not the section, to avoid sub-pixel seams at
        // column boundaries on fractional-DPR screens.
        "group-data-[variant=framed]/table:[&>tr]:bg-[color-mix(in_oklab,var(--muted)_55%,var(--card))]",
        "group-data-[variant=plain]/table:[&>tr]:bg-background",
        "group-data-[sticky-header]/table:sticky group-data-[sticky-header]/table:top-0 group-data-[sticky-header]/table:z-10",
        className,
      )}
      {...props}
    />
  )
}

function TableBody({ className, ...props }: React.ComponentProps<"tbody">) {
  return (
    <tbody
      data-slot="table-body"
      className={cn(
        // The frame already draws the bottom edge.
        "group-data-[variant=framed]/table:[&>tr:last-child>td]:border-b-0",
        className,
      )}
      {...props}
    />
  )
}

function TableFooter({ className, ...props }: React.ComponentProps<"tfoot">) {
  return (
    <tfoot
      data-slot="table-footer"
      className={cn(
        "font-medium [&>tr>td]:border-t [&>tr>td]:border-border/60 [&>tr:last-child>td]:border-b-0",
        "group-data-[variant=framed]/table:[&>tr]:bg-[color-mix(in_oklab,var(--muted)_55%,var(--card))]",
        className,
      )}
      {...props}
    />
  )
}

// ─── Row ──────────────────────────────────────────────────────────────────

/**
 * A table row. Set `data-state="selected"` to paint it as selected (the
 * shadcn / TanStack convention). Rows are a `group/row`, which is what
 * {@link TableRowActions} keys its hover reveal on.
 */
function TableRow({ className, ...props }: React.ComponentProps<"tr">) {
  return (
    <tr
      data-slot="table-row"
      className={cn(
        "group/row transition-colors",
        "[tbody>&]:hover:bg-muted/40 data-[state=selected]:bg-muted/70 [tbody>&]:data-[state=selected]:hover:bg-muted",
        className,
      )}
      {...props}
    />
  )
}

// ─── Cells ────────────────────────────────────────────────────────────────

type CellAlign = "start" | "center" | "end"

const alignClass: Record<CellAlign, string> = {
  start: "text-left",
  center: "text-center",
  end: "text-right",
}

// Shared horizontal rhythm: 12px between columns, 16px at the table edges.
// A leading checkbox column hugs its content.
const cellX =
  "px-3 first:pl-4 last:pr-4 [&:has([role=checkbox])]:w-px [&:has([role=checkbox])]:pr-0"

type SortDirection = "asc" | "desc" | false

interface TableHeadProps extends Omit<React.ComponentProps<"th">, "align"> {
  align?: CellAlign
  /**
   * Current sort of this column. Passing it (even `false`) makes the header a
   * sort button and sets `aria-sort`. Matches TanStack's
   * `column.getIsSorted()` return value.
   */
  sortDirection?: SortDirection
  /** Called when the sort button is pressed. */
  onSort?: (event: React.MouseEvent<HTMLButtonElement>) => void
}

function TableHead({
  align = "start",
  sortDirection,
  onSort,
  className,
  children,
  ...props
}: TableHeadProps) {
  const sortable = sortDirection !== undefined || onSort !== undefined
  const ariaSort = !sortable
    ? undefined
    : sortDirection === "asc"
      ? "ascending"
      : sortDirection === "desc"
        ? "descending"
        : "none"

  return (
    <th
      data-slot="table-head"
      aria-sort={ariaSort}
      className={cn(
        cellX,
        alignClass[align],
        "h-9 whitespace-nowrap border-b border-border/60 align-middle text-xs font-medium text-muted-foreground",
        "group-data-[density=compact]/table:h-8",
        className,
      )}
      {...props}
    >
      {sortable ? (
        <button
          type="button"
          onClick={onSort}
          className={cn(
            "group/sort -mx-1.5 inline-flex h-7 items-center gap-1 rounded-md px-1.5 outline-none transition-colors",
            "hover:bg-muted hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50",
            sortDirection && "text-foreground",
            align === "end" && "flex-row-reverse",
          )}
        >
          {children}
          {sortDirection === "asc" ? (
            <ArrowUp className="size-3.5 shrink-0" />
          ) : sortDirection === "desc" ? (
            <ArrowDown className="size-3.5 shrink-0" />
          ) : (
            <ChevronsUpDown className="size-3.5 shrink-0 opacity-0 transition-opacity group-hover/sort:opacity-60 group-focus-visible/sort:opacity-60" />
          )}
        </button>
      ) : (
        children
      )}
    </th>
  )
}

interface TableCellProps extends Omit<React.ComponentProps<"td">, "align"> {
  align?: CellAlign
}

function TableCell({ align = "start", className, ...props }: TableCellProps) {
  return (
    <td
      data-slot="table-cell"
      className={cn(
        cellX,
        alignClass[align],
        "h-11 whitespace-nowrap border-b border-border/60 align-middle",
        "group-data-[density=compact]/table:h-9 group-data-[density=comfortable]/table:h-14",
        className,
      )}
      {...props}
    />
  )
}

function TableCaption({ className, ...props }: React.ComponentProps<"caption">) {
  return (
    <caption
      data-slot="table-caption"
      className={cn(
        "mt-3 text-sm text-muted-foreground",
        "group-data-[variant=framed]/table:mt-0 group-data-[variant=framed]/table:border-t group-data-[variant=framed]/table:border-border/60 group-data-[variant=framed]/table:px-4 group-data-[variant=framed]/table:py-2.5 group-data-[variant=framed]/table:text-left group-data-[variant=framed]/table:text-xs",
        className,
      )}
      {...props}
    />
  )
}

// ─── Extras ───────────────────────────────────────────────────────────────

/**
 * Trailing row actions (edit, delete, a `…` menu). Hidden until the row is
 * hovered, focused within, or selected, so a column of icons doesn't compete
 * with the data. Always visible on devices without hover.
 *
 * Put it in a `<TableCell align="end">`; give that cell a fixed width
 * (`w-px` hugs the content) so the column doesn't absorb spare space.
 */
function TableRowActions({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="table-row-actions"
      className={cn(
        "-my-1 inline-flex items-center justify-end gap-1 transition-opacity",
        "[@media(hover:hover)]:opacity-0 group-hover/row:opacity-100 group-focus-within/row:opacity-100 group-data-[state=selected]/row:opacity-100",
        className,
      )}
      {...props}
    />
  )
}

interface TableEmptyProps extends Omit<React.ComponentProps<"tr">, "title"> {
  /** Number of columns to span — the table's column count. */
  colSpan: number
  icon?: React.ReactNode
  title?: React.ReactNode
  description?: React.ReactNode
  /** A call to action, typically a small button. */
  action?: React.ReactNode
}

/**
 * The body's empty state: one full-width row with an optional icon, title,
 * description, and action. Pass `children` instead for a fully custom body.
 */
function TableEmpty({
  colSpan,
  icon,
  title = "Nothing here yet",
  description,
  action,
  className,
  children,
  ...props
}: TableEmptyProps) {
  return (
    <tr data-slot="table-empty" className={className} {...props}>
      <td colSpan={colSpan} className="border-b border-border/60 px-4 py-10 text-center">
        {children ?? (
          <div className="mx-auto flex max-w-sm flex-col items-center gap-1.5">
            {icon && (
              <span className="mb-1 flex size-9 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-[18px]">
                {icon}
              </span>
            )}
            <div className="text-sm font-medium">{title}</div>
            {description && (
              <div className="text-sm text-muted-foreground">{description}</div>
            )}
            {action && <div className="mt-2">{action}</div>}
          </div>
        )}
      </td>
    </tr>
  )
}

interface TableLoadingProps {
  /** Number of placeholder rows. */
  rows?: number
  /** Number of columns — the table's column count. */
  columns: number
}

/** Placeholder rows shown while data loads. Render inside `<TableBody>`. */
function TableLoading({ rows = 3, columns }: TableLoadingProps) {
  // Vary bar widths so the skeleton doesn't read as a grid of identical pills.
  const widths = ["w-3/5", "w-2/5", "w-4/5", "w-1/2"]
  return (
    <>
      {Array.from({ length: rows }, (_, r) => (
        <tr key={r} data-slot="table-loading" aria-hidden>
          {Array.from({ length: columns }, (_, c) => (
            <TableCell key={c}>
              <div
                className={cn(
                  "inline-block h-3 max-w-40 animate-pulse rounded-full bg-muted-foreground/15 align-middle",
                  widths[(r + c) % widths.length],
                )}
              />
            </TableCell>
          ))}
        </tr>
      ))}
    </>
  )
}

// ─── Exports ──────────────────────────────────────────────────────────────

export {
  Table,
  TableHeader,
  TableBody,
  TableFooter,
  TableHead,
  TableRow,
  TableCell,
  TableCaption,
  TableRowActions,
  TableEmpty,
  TableLoading,
}
export type {
  TableProps,
  TableHeadProps,
  TableCellProps,
  TableEmptyProps,
  TableLoadingProps,
  TableVariant,
  TableDensity,
  SortDirection,
}
