/**
 * The main-thread half of the plugin UI runtime.
 *
 * A {@link PluginUiRuntime} runs one plugin module in its own Worker and keeps
 * the widget tree the module builds. It is surface-agnostic: a surface hands
 * it the module URL, the export to enter through with its context, and the
 * function that carries RPC calls to the backend. Rendering the tree is the
 * caller's job (`components/plugin-ui/`); this file holds no React.
 *
 * The runtime is the plugin's only route out. It times every call into the
 * plugin and terminates the Worker past {@link CALL_TIMEOUT_MS}, re-validates
 * each batch before applying it, and makes every network request itself, so
 * the module never sees the session.
 */

import {
  CALL_TIMEOUT_MS,
  ROOT_HANDLE,
  type FromWorker,
  type LogLevel,
  type Op,
  type RpcReply,
  type SurfaceEntry,
  type ToWorker,
} from "./protocol"
import {
  KINDS,
  acceptsEvent,
  checkCell,
  checkProp,
  isCreatable,
  type EventName,
  type Json,
} from "./widgets"

export interface WidgetNode {
  readonly handle: number
  readonly kind: string
  props: Record<string, Json>
  children: number[]
  /** Cell hints each child was added with, by child handle. */
  cells: Map<number, Json>
  parent: number | null
  listeners: Map<EventName, number[]>
}

export type RuntimeStatus =
  | { state: "loading" }
  | { state: "running" }
  /** The plugin broke: it failed to load, trapped, hung or sent a bad batch. */
  | { state: "faulted"; reason: string }
  /** The user aborted it. */
  | { state: "aborted" }

/** Carries one call to the plugin's backend. Rejecting counts as `unavailable`. */
export type RpcTransport = (fn: string, args: Json, signal: AbortSignal) => Promise<RpcReply>

export interface RuntimeOptions {
  /** Where the module is served. */
  url: string
  entry: SurfaceEntry
  rpc: RpcTransport
  onLog?: (level: LogLevel, message: string) => void
}

class InvalidBatch extends Error {}

export class PluginUiRuntime {
  private readonly options: RuntimeOptions
  private worker: Worker | null = null
  private watchdog: ReturnType<typeof setTimeout> | null = null
  private readonly calls = new Map<number, AbortController>()
  private readonly nodes = new Map<number, WidgetNode>()
  private readonly subscribers = new Set<() => void>()
  private version = 0
  private current: RuntimeStatus = { state: "loading" }

  constructor(options: RuntimeOptions) {
    this.options = options
  }

  get status(): RuntimeStatus {
    return this.current
  }

  /** Starts the module afresh; a runtime that was disposed may be started again. */
  start() {
    if (this.worker) return
    this.nodes.clear()
    this.nodes.set(ROOT_HANDLE, blank(ROOT_HANDLE, "root"))
    this.current = { state: "loading" }
    const worker = new Worker(new URL("./worker.ts", import.meta.url), {
      type: "module",
      name: "faber-plugin-ui",
    })
    worker.onmessage = (event: MessageEvent<FromWorker>) => this.receive(event.data)
    worker.onerror = (event) => this.end({ state: "faulted", reason: event.message || "the UI Worker failed" })
    this.worker = worker
    // A relative URL would resolve against the Worker's script, not the page.
    const url = new URL(this.options.url, document.baseURI).href
    this.send({ type: "start", url, entry: this.options.entry })
  }

  /** Stops the plugin at the user's request. Its widgets stay until the caller unmounts them. */
  abort() {
    this.end({ state: "aborted" })
  }

  /** Stops the plugin without reporting anything, for unmount. */
  dispose() {
    this.teardown()
  }

  // -------------------------------------------------------------------------
  // For the renderer
  // -------------------------------------------------------------------------

  subscribe = (notify: () => void): (() => void) => {
    this.subscribers.add(notify)
    return () => this.subscribers.delete(notify)
  }

  /** Changes whenever the tree or the status does. */
  getVersion = (): number => this.version

  node(handle: number): WidgetNode | undefined {
    return this.nodes.get(handle)
  }

  /**
   * Records a value the user changed on a host widget, so it renders at once
   * without waiting for the plugin to set it back, and tells the plugin.
   */
  change(handle: number, key: string, value: Json, event: EventName = "change") {
    const node = this.nodes.get(handle)
    if (!node || !checkProp(node.kind, key, value).ok) return
    node.props = { ...node.props, [key]: value }
    this.bump()
    this.emit(handle, event, { value })
  }

  /** Delivers `event` to every listener the plugin registered on `handle` for it. */
  emit(handle: number, event: EventName, payload: Json = {}) {
    if (this.current.state !== "running") return
    for (const listener of this.nodes.get(handle)?.listeners.get(event) ?? []) {
      this.send({ type: "event", listener, payload })
    }
  }

  /** Whether the plugin listens for `event` on `handle`, so the host can skip work it would ignore. */
  listens(handle: number, event: EventName): boolean {
    return (this.nodes.get(handle)?.listeners.get(event)?.length ?? 0) > 0
  }

  // -------------------------------------------------------------------------
  // Worker traffic
  // -------------------------------------------------------------------------

  private send(message: ToWorker) {
    this.worker?.postMessage(message)
  }

  private receive(message: FromWorker) {
    switch (message.type) {
      case "enter":
        this.arm()
        break
      case "exit":
        this.disarm()
        try {
          for (const op of message.ops) this.apply(op)
        } catch (error) {
          if (!(error instanceof InvalidBatch)) throw error
          this.end({ state: "faulted", reason: `the plugin sent an invalid update: ${error.message}` })
          return
        }
        if (this.current.state === "loading") this.current = { state: "running" }
        this.bump()
        break
      case "rpc":
        void this.call(message.id, message.fn, message.args)
        break
      case "rpc-cancel":
        this.calls.get(message.id)?.abort()
        this.calls.delete(message.id)
        break
      case "log":
        ;(this.options.onLog ?? consoleLog(this.options.entry.surface))(message.level, message.message)
        break
      case "fault":
        this.end({ state: "faulted", reason: message.message })
        break
    }
  }

  private arm() {
    this.disarm()
    this.watchdog = setTimeout(
      () =>
        this.end({
          state: "faulted",
          reason: `the plugin did not return within ${CALL_TIMEOUT_MS / 1000} seconds`,
        }),
      CALL_TIMEOUT_MS,
    )
  }

  private disarm() {
    if (this.watchdog !== null) clearTimeout(this.watchdog)
    this.watchdog = null
  }

  private async call(id: number, fn: string, args: Json) {
    const controller = new AbortController()
    this.calls.set(id, controller)
    let reply: RpcReply
    try {
      reply = await this.options.rpc(fn, args, controller.signal)
    } catch (error) {
      reply = controller.signal.aborted
        ? { error: { code: "cancelled", message: "the call was cancelled" } }
        : {
            error: {
              code: "unavailable",
              message: error instanceof Error ? error.message : "the call did not complete",
            },
          }
    }
    if (this.calls.get(id) !== controller) return // cancelled, or the runtime ended
    this.calls.delete(id)
    this.send({ type: "reply", id, reply })
  }

  private end(status: RuntimeStatus) {
    if (this.current.state === "faulted" || this.current.state === "aborted") return
    this.teardown()
    this.current = status
    this.bump()
  }

  private teardown() {
    this.disarm()
    this.worker?.terminate()
    this.worker = null
    for (const controller of this.calls.values()) controller.abort()
    this.calls.clear()
  }

  private bump() {
    this.version++
    for (const notify of this.subscribers) notify()
  }

  // -------------------------------------------------------------------------
  // Applying a batch
  // -------------------------------------------------------------------------

  /**
   * Applies one op, checking it again: the Worker has already validated it,
   * but this runs in Faber's own page and the Worker's word is not enough.
   */
  private apply(op: Op) {
    switch (op.op) {
      case "create": {
        if (!isCreatable(op.kind)) throw new InvalidBatch(`unknown kind ${op.kind}`)
        if (this.nodes.has(op.handle)) throw new InvalidBatch(`widget ${op.handle} exists`)
        this.nodes.set(op.handle, blank(op.handle, op.kind))
        break
      }
      case "destroy": {
        const node = this.must(op.handle)
        if (node.handle === ROOT_HANDLE) throw new InvalidBatch("the root cannot be destroyed")
        this.detach(node)
        this.forget(node)
        break
      }
      case "set": {
        const node = this.must(op.handle)
        const checked = checkProp(node.kind, op.key, op.value)
        if (!checked.ok) throw new InvalidBatch(checked.error)
        node.props = { ...node.props, [op.key]: checked.value }
        break
      }
      case "add": {
        const parent = this.must(op.parent)
        const child = this.must(op.child)
        if (!KINDS[parent.kind].container) throw new InvalidBatch(`${parent.kind} cannot hold children`)
        if (child.handle === ROOT_HANDLE) throw new InvalidBatch("the root cannot be added")
        for (let at: WidgetNode | undefined = parent; at; at = this.parentOf(at)) {
          if (at === child) throw new InvalidBatch(`widget ${child.handle} inside itself`)
        }
        const cell = checkCell(parent.kind, op.cell)
        if (!cell.ok) throw new InvalidBatch(cell.error)
        this.detach(child)
        child.parent = parent.handle
        parent.children = [...parent.children, child.handle]
        parent.cells.set(child.handle, cell.value)
        break
      }
      case "remove": {
        const child = this.must(op.child)
        if (child.parent !== op.parent) throw new InvalidBatch(`widget ${op.child} is not in ${op.parent}`)
        this.detach(child)
        break
      }
      case "listen": {
        const node = this.must(op.handle)
        if (!acceptsEvent(node.kind, op.event)) throw new InvalidBatch(`${node.kind} has no ${op.event} event`)
        node.listeners.set(op.event, [...(node.listeners.get(op.event) ?? []), op.listener])
        break
      }
    }
  }

  private must(handle: number): WidgetNode {
    const node = this.nodes.get(handle)
    if (!node) throw new InvalidBatch(`no widget ${handle}`)
    return node
  }

  private parentOf(node: WidgetNode): WidgetNode | undefined {
    return node.parent === null ? undefined : this.nodes.get(node.parent)
  }

  private detach(node: WidgetNode) {
    const parent = this.parentOf(node)
    node.parent = null
    if (!parent) return
    parent.children = parent.children.filter((handle) => handle !== node.handle)
    parent.cells.delete(node.handle)
  }

  private forget(node: WidgetNode) {
    for (const child of node.children) {
      const found = this.nodes.get(child)
      if (found) this.forget(found)
    }
    this.nodes.delete(node.handle)
  }
}

function blank(handle: number, kind: string): WidgetNode {
  return {
    handle,
    kind,
    props: {},
    children: [],
    cells: new Map(),
    parent: null,
    listeners: new Map(),
  }
}

function consoleLog(surface: string) {
  return (level: LogLevel, message: string) => {
    const line = `[plugin ui:${surface}] ${message}`
    if (level === "error") console.error(line)
    else if (level === "warn") console.warn(line)
    else if (level === "info") console.info(line)
    else console.debug(line)
  }
}
