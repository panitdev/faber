import * as React from "react"
import { Tabs as TabsPrimitive } from "radix-ui"
import {
  CircleAlert,
  CircleCheck,
  FolderOpen,
  Info,
  Loader2,
  TriangleAlert,
} from "lucide-react"

import { cn } from "@/lib/utils"
import type { PluginUiRuntime, WidgetNode } from "@/lib/plugin-ui/runtime"
import { boxStyle, scrollStyle, textStyle } from "@/lib/plugin-ui/style"
import type { Json } from "@/lib/plugin-ui/widgets"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableEmpty,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

/**
 * Renders a plugin's widget tree. Every widget is Faber's own component; the
 * plugin only ever chose which ones, with which validated properties.
 */

interface Host {
  runtime: PluginUiRuntime
  /** Names the plugin on anything that leaves its frame, such as a dialog. */
  pluginName: string
}

const HostContext = React.createContext<Host | null>(null)

export function WidgetHost({
  runtime,
  pluginName,
  children,
}: Host & { children: React.ReactNode }) {
  const host = React.useMemo(() => ({ runtime, pluginName }), [runtime, pluginName])
  return <HostContext.Provider value={host}>{children}</HostContext.Provider>
}

function useHost(): Host {
  const host = React.useContext(HostContext)
  if (!host) throw new Error("plugin widgets render inside a WidgetHost")
  return host
}

export function Widget({ handle }: { handle: number }) {
  const { runtime } = useHost()
  const node = runtime.node(handle)
  if (!node) return null
  const View = VIEWS[node.kind]
  return View ? <View node={node} /> : null
}

function Children({ node }: { node: WidgetNode }) {
  return node.children.map((child) => <Widget key={child} handle={child} />)
}

type View = (props: { node: WidgetNode }) => React.ReactNode

const str = (value: Json | undefined): string | undefined =>
  typeof value === "string" ? value : undefined

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/** Pointer and key handlers for a primitive, wired only for events the plugin listens to. */
function usePrimitiveEvents(node: WidgetNode) {
  const { runtime } = useHost()
  const listens = (event: "click" | "hover" | "key") => runtime.listens(node.handle, event)
  const focusable = node.props.focusable === true
  return {
    onClick: listens("click") ? () => runtime.emit(node.handle, "click") : undefined,
    onPointerEnter: listens("hover")
      ? () => runtime.emit(node.handle, "hover", { inside: true })
      : undefined,
    onPointerLeave: listens("hover")
      ? () => runtime.emit(node.handle, "hover", { inside: false })
      : undefined,
    onKeyDown:
      focusable && listens("key")
        ? (event: React.KeyboardEvent) =>
            runtime.emit(node.handle, "key", {
              key: event.key.slice(0, 32),
              shift: event.shiftKey,
              ctrl: event.ctrlKey,
              alt: event.altKey,
              meta: event.metaKey,
            })
        : undefined,
    tabIndex: focusable ? 0 : undefined,
    role: str(node.props.role),
    "aria-label": str(node.props["aria-label"]),
  }
}

const BoxView: View = ({ node }) => (
  <div
    data-plugin-widget="box"
    style={boxStyle(node.props)}
    className="outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
    {...usePrimitiveEvents(node)}
  >
    <Children node={node} />
  </div>
)

const TextView: View = ({ node }) => (
  <span data-plugin-widget="text" style={textStyle(node.props)} {...usePrimitiveEvents(node)}>
    {str(node.props.text)}
  </span>
)

const RootView: View = ({ node }) => (
  <div data-plugin-widget="root" className="flex min-w-0 flex-col gap-4">
    <Children node={node} />
  </div>
)

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

const ScrollView: View = ({ node }) => (
  <div data-plugin-widget="scroll" className="flex min-h-0 flex-col overflow-auto" style={scrollStyle(node.props)}>
    <Children node={node} />
  </div>
)

const TabsView: View = ({ node }) => {
  const { runtime } = useHost()
  const tabs = node.children.map((child) => {
    const cell = node.cells.get(child) as { title: string; value?: string }
    return { child, title: cell.title, value: cell.value ?? cell.title }
  })
  const value = str(node.props.value) ?? tabs[0]?.value
  return (
    <TabsPrimitive.Root
      value={value}
      onValueChange={(next) => runtime.change(node.handle, "value", next)}
      className="flex min-w-0 flex-col gap-3"
    >
      <TabsPrimitive.List className="inline-flex w-fit gap-1 rounded-full bg-muted p-1">
        {tabs.map((tab) => (
          <TabsPrimitive.Trigger
            key={tab.child}
            value={tab.value}
            className="rounded-full px-3 py-1 text-sm text-muted-foreground outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 data-[state=active]:bg-card data-[state=active]:text-foreground data-[state=active]:shadow-sm"
          >
            {tab.title}
          </TabsPrimitive.Trigger>
        ))}
      </TabsPrimitive.List>
      {tabs.map((tab) => (
        <TabsPrimitive.Content key={tab.child} value={tab.value} className="outline-none">
          <Widget handle={tab.child} />
        </TabsPrimitive.Content>
      ))}
    </TabsPrimitive.Root>
  )
}

/**
 * A dialog opens over the whole page, outside the plugin's frame, so it
 * always says which plugin it came from. Its modal overlay covers the frame's
 * abort control, and the plugin may answer `close` by opening it again, so
 * the dialog carries its own host-drawn way to stop the plugin.
 */
const DialogView: View = ({ node }) => {
  const { runtime, pluginName } = useHost()
  return (
    <Dialog
      open={node.props.open === true}
      onOpenChange={(open) => {
        if (!open) runtime.change(node.handle, "open", false, "close")
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{str(node.props.title) ?? pluginName}</DialogTitle>
          <DialogDescription>
            {str(node.props.description) ?? `From the ${pluginName} plugin`}
          </DialogDescription>
        </DialogHeader>
        <div className="flex min-w-0 flex-col gap-4">
          <Children node={node} />
        </div>
        <div className="flex items-center justify-between gap-3 border-t border-border/60 pt-3">
          <p className="text-xs text-muted-foreground">
            {node.props.description !== undefined && `From the ${pluginName} plugin`}
          </p>
          <Button variant="ghost" size="xs" onClick={() => runtime.abort()}>
            Stop {pluginName}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

const LabelView: View = ({ node }) => (
  <p className={cn("text-sm", node.props.muted === true && "text-muted-foreground")}>
    {str(node.props.text)}
  </p>
)

const BADGE_TONE: Record<string, string> = {
  default: "bg-primary text-primary-foreground border-transparent",
  secondary: "bg-secondary text-secondary-foreground border-transparent",
  danger: "bg-destructive/10 text-destructive border-destructive/20",
  outline: "border-border text-foreground",
}

const BadgeView: View = ({ node }) => (
  <span
    className={cn(
      "inline-flex w-fit items-center rounded-full border px-2 py-0.5 text-xs font-medium",
      BADGE_TONE[str(node.props.tone) ?? "default"],
    )}
  >
    {str(node.props.text)}
  </span>
)

const ALERT_TONE = {
  info: { icon: Info, className: "border-border bg-card text-foreground" },
  success: { icon: CircleCheck, className: "border-border bg-card text-foreground" },
  warning: { icon: TriangleAlert, className: "border-border bg-muted text-foreground" },
  danger: {
    icon: CircleAlert,
    className: "border-destructive/30 bg-destructive/5 text-destructive",
  },
} as const

const AlertView: View = ({ node }) => {
  const tone = ALERT_TONE[(str(node.props.tone) ?? "info") as keyof typeof ALERT_TONE]
  const Icon = tone.icon
  return (
    <div role="alert" className={cn("flex gap-3 rounded-xl border px-4 py-3 text-sm", tone.className)}>
      <Icon className="mt-0.5 size-4 shrink-0" />
      <div className="flex min-w-0 flex-col gap-0.5">
        {node.props.title !== undefined && <p className="font-medium">{str(node.props.title)}</p>}
        {node.props.text !== undefined && (
          <p className="whitespace-pre-wrap opacity-90">{str(node.props.text)}</p>
        )}
      </div>
    </div>
  )
}

const SpinnerView: View = ({ node }) => (
  <div role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
    <Loader2 className="size-4 animate-spin" />
    {str(node.props.text)}
  </div>
)

const DividerView: View = () => <hr className="border-border" />

const CodeView: View = ({ node }) => (
  <pre className="overflow-auto rounded-lg bg-muted px-3 py-2 font-mono text-xs leading-relaxed">
    <code>{str(node.props.text)}</code>
  </pre>
)

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

const BUTTON_VARIANT = {
  default: "default",
  outline: "outline",
  secondary: "secondary",
  ghost: "ghost",
  danger: "destructive",
} as const

const ButtonView: View = ({ node }) => {
  const { runtime } = useHost()
  return (
    <Button
      size="sm"
      className="w-fit"
      variant={BUTTON_VARIANT[(str(node.props.variant) ?? "default") as keyof typeof BUTTON_VARIANT]}
      disabled={node.props.disabled === true}
      loading={node.props.loading === true}
      onClick={() => runtime.emit(node.handle, "click")}
    >
      {str(node.props.text)}
    </Button>
  )
}

/** Label above, error below, shared by every input widget. */
function Field({
  node,
  id,
  children,
}: {
  node: WidgetNode
  id: string
  children: React.ReactNode
}) {
  const error = str(node.props.error)
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      {node.props.label !== undefined && (
        <label htmlFor={id} className="text-sm font-medium">
          {str(node.props.label)}
        </label>
      )}
      {children}
      {error && <p className="text-xs text-destructive">{error}</p>}
    </div>
  )
}

const inputClass =
  "h-10 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 text-base transition-colors outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:bg-input/50 disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 md:text-sm dark:bg-input/30"

const SwitchView: View = ({ node }) => {
  const { runtime } = useHost()
  const id = React.useId()
  const error = str(node.props.error)
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <div className="flex items-center gap-3">
        <Switch
          id={id}
          checked={node.props.checked === true}
          disabled={node.props.disabled === true}
          aria-invalid={error ? true : undefined}
          onCheckedChange={(checked) => runtime.change(node.handle, "checked", checked)}
        />
        {node.props.label !== undefined && (
          <label htmlFor={id} className="text-sm">
            {str(node.props.label)}
          </label>
        )}
      </div>
      {error && <p className="text-xs text-destructive">{error}</p>}
    </div>
  )
}

const InputView: View = ({ node }) => {
  const { runtime } = useHost()
  const id = React.useId()
  return (
    <Field node={node} id={id}>
      <input
        id={id}
        type={node.props.secret === true ? "password" : "text"}
        autoComplete="off"
        className={inputClass}
        value={str(node.props.value) ?? ""}
        placeholder={str(node.props.placeholder)}
        disabled={node.props.disabled === true}
        aria-invalid={str(node.props.error) ? true : undefined}
        onChange={(event) => runtime.change(node.handle, "value", event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            runtime.emit(node.handle, "submit", { value: str(node.props.value) ?? "" })
          }
        }}
      />
    </Field>
  )
}

const TextareaView: View = ({ node }) => {
  const { runtime } = useHost()
  const id = React.useId()
  return (
    <Field node={node} id={id}>
      <Textarea
        id={id}
        value={str(node.props.value) ?? ""}
        placeholder={str(node.props.placeholder)}
        rows={typeof node.props.rows === "number" ? node.props.rows : undefined}
        disabled={node.props.disabled === true}
        aria-invalid={str(node.props.error) ? true : undefined}
        onChange={(event) => runtime.change(node.handle, "value", event.target.value)}
      />
    </Field>
  )
}

const SelectView: View = ({ node }) => {
  const { runtime } = useHost()
  const id = React.useId()
  const options = (node.props.options ?? []) as { value: string; label?: string }[]
  return (
    <Field node={node} id={id}>
      <Select
        value={str(node.props.value) ?? ""}
        disabled={node.props.disabled === true}
        onValueChange={(value) => runtime.change(node.handle, "value", value)}
      >
        <SelectTrigger id={id} size="sm" aria-invalid={str(node.props.error) ? true : undefined}>
          <SelectValue placeholder={str(node.props.placeholder)} />
        </SelectTrigger>
        <SelectContent>
          {options
            .filter((option) => option.value !== "")
            .map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label ?? option.value}
              </SelectItem>
            ))}
        </SelectContent>
      </Select>
    </Field>
  )
}

/**
 * Keeps what the user is typing as text, so a half-typed number such as `-`
 * or `1.` survives, and reports a number (or null when empty) to the plugin.
 */
const NumberView: View = ({ node }) => {
  const { runtime } = useHost()
  const id = React.useId()
  const value = typeof node.props.value === "number" ? node.props.value : null
  const [draft, setDraft] = React.useState(value === null ? "" : String(value))
  const [shown, setShown] = React.useState(value)
  if (value !== shown) {
    setShown(value)
    if (value !== (draft.trim() === "" ? null : Number(draft))) {
      setDraft(value === null ? "" : String(value))
    }
  }
  const num = (key: string) => (typeof node.props[key] === "number" ? (node.props[key] as number) : undefined)
  return (
    <Field node={node} id={id}>
      <input
        id={id}
        type="number"
        inputMode="decimal"
        className={inputClass}
        value={draft}
        min={num("min")}
        max={num("max")}
        step={num("step")}
        placeholder={str(node.props.placeholder)}
        disabled={node.props.disabled === true}
        aria-invalid={str(node.props.error) ? true : undefined}
        onChange={(event) => {
          setDraft(event.target.value)
          const parsed = event.target.value.trim() === "" ? null : event.target.valueAsNumber
          if (parsed === null || Number.isFinite(parsed)) {
            setShown(parsed)
            runtime.change(node.handle, "value", parsed)
          }
        }}
      />
    </Field>
  )
}

/**
 * A path inside an environment. Typed for now; browsing the environment
 * through the host is not wired yet.
 */
const PathView: View = ({ node }) => {
  const { runtime } = useHost()
  const id = React.useId()
  return (
    <Field node={node} id={id}>
      <div className="relative">
        <FolderOpen className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
        <input
          id={id}
          type="text"
          spellCheck={false}
          autoComplete="off"
          className={cn(inputClass, "pl-8 font-mono")}
          value={str(node.props.value) ?? ""}
          placeholder={str(node.props.placeholder)}
          disabled={node.props.disabled === true}
          aria-invalid={str(node.props.error) ? true : undefined}
          onChange={(event) => runtime.change(node.handle, "value", event.target.value)}
        />
      </div>
    </Field>
  )
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/** Children are placed by their cell hints: rows in the order their first cell arrived. */
const TableView: View = ({ node }) => {
  const columns = (node.props.columns ?? []) as string[]
  const rows = new Map<string, Map<number, number>>()
  for (const child of node.children) {
    const cell = node.cells.get(child) as { row: string; column: number }
    let row = rows.get(cell.row)
    if (!row) rows.set(cell.row, (row = new Map()))
    row.set(cell.column, child)
  }
  const width = Math.max(
    columns.length,
    ...[...rows.values()].map((row) => Math.max(0, ...row.keys()) + 1),
  )
  return (
    <Table density="compact">
      {columns.length > 0 && (
        <TableHeader>
          <TableRow>
            {Array.from({ length: width }, (_, column) => (
              <TableHead key={column}>{columns[column] ?? ""}</TableHead>
            ))}
          </TableRow>
        </TableHeader>
      )}
      <TableBody>
        {rows.size === 0 ? (
          <TableEmpty colSpan={Math.max(width, 1)} title={str(node.props.empty) ?? "Nothing here yet"} />
        ) : (
          [...rows].map(([key, row]) => (
            <TableRow key={key}>
              {Array.from({ length: width }, (_, column) => {
                const child = row.get(column)
                return <TableCell key={column}>{child !== undefined && <Widget handle={child} />}</TableCell>
              })}
            </TableRow>
          ))
        )}
      </TableBody>
    </Table>
  )
}

const ListView: View = ({ node }) =>
  node.children.length === 0 ? (
    <p className="text-sm text-muted-foreground">{str(node.props.empty) ?? "Nothing here yet"}</p>
  ) : (
    <ul className="flex flex-col divide-y divide-border rounded-xl border border-border/60 bg-card">
      {node.children.map((child) => (
        <li key={child} className="min-w-0 px-4 py-3">
          <Widget handle={child} />
        </li>
      ))}
    </ul>
  )

const VIEWS: Record<string, View> = {
  root: RootView,
  box: BoxView,
  text: TextView,
  scroll: ScrollView,
  tabs: TabsView,
  dialog: DialogView,
  label: LabelView,
  badge: BadgeView,
  alert: AlertView,
  spinner: SpinnerView,
  divider: DividerView,
  code: CodeView,
  button: ButtonView,
  switch: SwitchView,
  input: InputView,
  textarea: TextareaView,
  select: SelectView,
  number: NumberView,
  path: PathView,
  table: TableView,
  list: ListView,
}
