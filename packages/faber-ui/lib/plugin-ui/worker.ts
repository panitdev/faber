/**
 * The plugin UI Worker: one fixed host for any plugin's `ui.wasm`.
 *
 * It instantiates the module with the `faber_ui` imports and nothing else, so
 * the module can only reach what is listed here. Every import call is checked
 * against the widget vocabulary and recorded as an op; when a call into the
 * plugin returns, the ops go to the main thread as one batch. The main thread
 * runs the watchdog, since a Worker stuck in a call cannot time itself out.
 *
 * ABI, in the plugin's linear memory (strings and JSON as UTF-8 `(ptr, len)`):
 *
 *   imports, module "faber_ui"
 *     ui_create(kind_ptr, kind_len) -> u32         handle, 0 on error
 *     ui_destroy(w)                                 the subtree and its listeners
 *     ui_set(w, key_ptr, key_len, val_ptr, val_len) -> i32
 *                                                   0 ok, -1 unknown widget,
 *                                                   -2 unknown key, -3 invalid
 *                                                   value, -4 value not JSON
 *     ui_add(parent, child, cell_ptr, cell_len)     cell hints as JSON, may be empty
 *     ui_remove(parent, child)
 *     ui_on(w, event_ptr, event_len) -> u32         listener id, 0 on error
 *     rpc_call(fn_ptr, fn_len, args_ptr, args_len) -> u32   call id
 *     rpc_cancel(id)
 *     log(level, ptr, len)                          0 error, 1 warn, 2 info, 3 debug
 *
 *   exports
 *     memory
 *     alloc(len) -> ptr                             the host writes payloads here
 *     free(ptr, len)                                and frees them once the call returns
 *     on_event(listener, ptr, len)                  required if it imports ui_on
 *     on_reply(id, status, ptr, len)                required if it imports rpc_call;
 *                                                   0 ok, 1 error; 2 reserved for partial
 *     <entry>(ptr, len)                             the surface's own, called once with
 *                                                   its context; `context.root` is the
 *                                                   handle to build under
 *
 * Errors from `ui_*` also go to the plugin's log, so they show up during
 * development without the plugin checking every return.
 */

/// <reference lib="webworker" />

import {
  RPC_PAYLOAD_LIMIT,
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
  type Json,
} from "./widgets"

const scope = self as unknown as DedicatedWorkerGlobalScope

/** Upper bound on live widgets, so a runaway loop fails instead of exhausting the page. */
const MAX_WIDGETS = 10_000
/** Upper bound on one string or JSON value read out of the plugin's memory. */
const MAX_READ = 2 * RPC_PAYLOAD_LIMIT
const MAX_LOG = 4096

const IMPORT_NAMES = [
  "ui_create",
  "ui_destroy",
  "ui_set",
  "ui_add",
  "ui_remove",
  "ui_on",
  "rpc_call",
  "rpc_cancel",
  "log",
] as const

interface PluginExports {
  memory: WebAssembly.Memory
  alloc(len: number): number
  free(ptr: number, len: number): void
  /** Present whenever the module imports `ui_on`. */
  on_event(listener: number, ptr: number, len: number): void
  /** Present whenever the module imports `rpc_call`. */
  on_reply(id: number, status: number, ptr: number, len: number): void
}

/** A broken ABI contract. It ends the plugin, like a trap does. */
class AbiError extends Error {}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

let plugin: PluginExports | null = null
let closed = false
let ops: Op[] = []

const kinds = new Map<number, string>()
const parents = new Map<number, number>()
const children = new Map<number, Set<number>>()
const listeners = new Map<number, number>() // listener -> widget
const listenersOf = new Map<number, Set<number>>()
const pendingCalls = new Set<number>()

let nextHandle = ROOT_HANDLE + 1
let nextListener = 1
let nextCall = 1

function post(message: FromWorker) {
  scope.postMessage(message)
}

function log(level: LogLevel, message: string) {
  post({ type: "log", level, message })
}

/** Reports the plugin as broken and closes the Worker. */
function end(message: string) {
  if (closed) return
  closed = true
  plugin = null
  post({ type: "fault", message })
  scope.close()
}

/** {@link end}, then unwinds out of whatever called into the plugin. */
function fault(message: string): never {
  end(message)
  throw new AbiError(message)
}

// ---------------------------------------------------------------------------
// Linear memory
// ---------------------------------------------------------------------------

/**
 * A view of `len` bytes at `ptr`, made fresh on every read: a `memory.grow`
 * inside the plugin detaches any view taken before it.
 */
function view(ptr: number, len: number): Uint8Array {
  const memory = plugin?.memory
  if (!memory) throw new AbiError("memory read outside a call")
  ptr >>>= 0
  len >>>= 0
  if (len > MAX_READ) throw new AbiError(`a ${len}-byte value is over the ${MAX_READ}-byte limit`)
  if (ptr + len > memory.buffer.byteLength) {
    throw new AbiError(`(${ptr}, ${len}) is outside linear memory`)
  }
  return new Uint8Array(memory.buffer, ptr, len)
}

function readString(ptr: number, len: number): string {
  return decoder.decode(view(ptr, len))
}

/** JSON at `(ptr, len)`; an empty range reads as null. Undefined if it does not parse. */
function readJson(ptr: number, len: number): Json | undefined {
  if (len >>> 0 === 0) return null
  try {
    return JSON.parse(readString(ptr, len)) as Json
  } catch (error) {
    if (error instanceof AbiError) throw error
    return undefined
  }
}

/** Copies `value` as JSON into memory the plugin allocated. */
function write(value: Json): [number, number] {
  const exports = plugin
  if (!exports) throw new AbiError("write outside a call")
  const bytes = encoder.encode(JSON.stringify(value))
  if (bytes.length === 0) return [0, 0]
  const ptr = exports.alloc(bytes.length) >>> 0
  view(ptr, bytes.length).set(bytes)
  return [ptr, bytes.length]
}

// ---------------------------------------------------------------------------
// Calling into the plugin
// ---------------------------------------------------------------------------

/**
 * Runs one call into the plugin between `enter` and `exit`, so the main
 * thread can time it, and flushes the ops it made. A trap ends the plugin.
 */
function enter(run: (exports: PluginExports) => void) {
  const exports = plugin
  if (!exports) return
  post({ type: "enter" })
  try {
    run(exports)
  } catch (error) {
    end(describe(error))
    return
  }
  const batch = ops
  ops = []
  post({ type: "exit", ops: batch })
}

/** Calls an export with `value` as its trailing `(ptr, len)`, then frees it. */
function withPayload(
  exports: PluginExports,
  value: Json,
  call: (ptr: number, len: number) => void,
) {
  const [ptr, len] = write(value)
  call(ptr, len)
  if (len > 0) exports.free(ptr, len)
}

function describe(error: unknown): string {
  if (error instanceof WebAssembly.RuntimeError) return `the plugin trapped: ${error.message}`
  if (error instanceof Error) return error.message
  return String(error)
}

// ---------------------------------------------------------------------------
// Widget bookkeeping
// ---------------------------------------------------------------------------

function detach(child: number) {
  const parent = parents.get(child)
  if (parent === undefined) return
  parents.delete(child)
  children.get(parent)?.delete(child)
}

function forget(handle: number) {
  for (const child of children.get(handle) ?? []) forget(child)
  children.delete(handle)
  parents.delete(handle)
  kinds.delete(handle)
  for (const listener of listenersOf.get(handle) ?? []) listeners.delete(listener)
  listenersOf.delete(handle)
}

function isAncestor(candidate: number, of: number): boolean {
  for (let at: number | undefined = of; at !== undefined; at = parents.get(at)) {
    if (at === candidate) return true
  }
  return false
}

// ---------------------------------------------------------------------------
// The faber_ui imports
// ---------------------------------------------------------------------------

const imports: Record<(typeof IMPORT_NAMES)[number], (...args: number[]) => number | void> = {
  ui_create(kindPtr, kindLen) {
    const kind = readString(kindPtr, kindLen)
    if (!isCreatable(kind)) {
      log("error", `ui_create: there is no widget kind "${kind}"`)
      return 0
    }
    if (kinds.size >= MAX_WIDGETS) {
      log("error", `ui_create: over ${MAX_WIDGETS} live widgets`)
      return 0
    }
    const handle = nextHandle++
    kinds.set(handle, kind)
    ops.push({ op: "create", handle, kind })
    return handle
  },

  ui_destroy(handle) {
    handle >>>= 0
    if (handle === ROOT_HANDLE) {
      log("error", "ui_destroy: the root belongs to the host")
      return
    }
    if (!kinds.has(handle)) {
      log("warn", `ui_destroy: no widget ${handle}`)
      return
    }
    detach(handle)
    forget(handle)
    ops.push({ op: "destroy", handle })
  },

  ui_set(handle, keyPtr, keyLen, valuePtr, valueLen) {
    handle >>>= 0
    const kind = kinds.get(handle)
    if (kind === undefined) {
      log("error", `ui_set: no widget ${handle}`)
      return -1
    }
    const key = readString(keyPtr, keyLen)
    const value = readJson(valuePtr, valueLen)
    if (value === undefined) {
      log("error", `ui_set: ${kind}.${key} is not JSON`)
      return -4
    }
    const checked = checkProp(kind, key, value)
    if (!checked.ok) {
      log("error", `ui_set: ${checked.error}`)
      return checked.code === "unknown_key" ? -2 : -3
    }
    ops.push({ op: "set", handle, key, value: checked.value })
    return 0
  },

  ui_add(parent, child, cellPtr, cellLen) {
    parent >>>= 0
    child >>>= 0
    const parentKind = kinds.get(parent)
    if (parentKind === undefined || !kinds.has(child)) {
      log("error", `ui_add: no widget ${parentKind === undefined ? parent : child}`)
      return
    }
    if (!KINDS[parentKind].container) {
      log("error", `ui_add: ${parentKind} cannot hold children`)
      return
    }
    if (isAncestor(child, parent)) {
      log("error", `ui_add: widget ${child} cannot be added inside itself`)
      return
    }
    const cell = readJson(cellPtr, cellLen)
    if (cell === undefined) {
      log("error", "ui_add: cell hints are not JSON")
      return
    }
    const checked = checkCell(parentKind, cell)
    if (!checked.ok) {
      log("error", `ui_add: ${checked.error}`)
      return
    }
    detach(child)
    parents.set(child, parent)
    let siblings = children.get(parent)
    if (!siblings) children.set(parent, (siblings = new Set()))
    siblings.add(child)
    ops.push({ op: "add", parent, child, cell: checked.value })
  },

  ui_remove(parent, child) {
    parent >>>= 0
    child >>>= 0
    if (parents.get(child) !== parent) {
      log("warn", `ui_remove: widget ${child} is not in widget ${parent}`)
      return
    }
    detach(child)
    ops.push({ op: "remove", parent, child })
  },

  ui_on(handle, eventPtr, eventLen) {
    handle >>>= 0
    const kind = kinds.get(handle)
    if (kind === undefined) {
      log("error", `ui_on: no widget ${handle}`)
      return 0
    }
    const event = readString(eventPtr, eventLen)
    if (!acceptsEvent(kind, event)) {
      log("error", `ui_on: ${kind} has no "${event}" event`)
      return 0
    }
    const listener = nextListener++
    listeners.set(listener, handle)
    let own = listenersOf.get(handle)
    if (!own) listenersOf.set(handle, (own = new Set()))
    own.add(listener)
    ops.push({ op: "listen", handle, event, listener })
    return listener
  },

  rpc_call(fnPtr, fnLen, argsPtr, argsLen) {
    const id = nextCall++
    const fn = readString(fnPtr, fnLen)
    if ((argsLen >>> 0) > RPC_PAYLOAD_LIMIT) {
      settleLater(id, hostError("too_large", `arguments over ${RPC_PAYLOAD_LIMIT} bytes`))
      return id
    }
    const args = readJson(argsPtr, argsLen)
    if (args === undefined) {
      log("error", `rpc_call: arguments to ${fn} are not JSON`)
      settleLater(id, hostError("not_allowed", "arguments are not JSON"))
      return id
    }
    pendingCalls.add(id)
    post({ type: "rpc", id, fn, args })
    return id
  },

  rpc_cancel(id) {
    id >>>= 0
    if (pendingCalls.delete(id)) post({ type: "rpc-cancel", id })
  },

  log(level, ptr, len) {
    const levels: LogLevel[] = ["error", "warn", "info", "debug"]
    const text = readString(ptr, Math.min(len >>> 0, MAX_LOG))
    log(levels[level >>> 0] ?? "info", text)
  },
}

function hostError(code: string, message: string): RpcReply {
  return { error: { code, message } }
}

/** A reply the Worker makes itself, delivered after the current call returns. */
function settleLater(id: number, reply: RpcReply) {
  pendingCalls.add(id)
  setTimeout(() => settle(id, reply), 0)
}

function settle(id: number, reply: RpcReply) {
  if (!pendingCalls.delete(id)) return // cancelled
  let status = "ok" in reply ? 0 : 1
  let payload: Json = "ok" in reply ? reply.ok : reply.error
  if (encoder.encode(JSON.stringify(payload)).length > RPC_PAYLOAD_LIMIT) {
    status = 1
    payload = { code: "too_large", message: `reply over ${RPC_PAYLOAD_LIMIT} bytes` }
  }
  enter((exports) =>
    withPayload(exports, payload, (ptr, len) => exports.on_reply(id, status, ptr, len)),
  )
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

async function compile(url: string): Promise<WebAssembly.Module> {
  const response = await fetch(url, { credentials: "same-origin" })
  if (!response.ok) throw new Error(`fetching the UI module failed with ${response.status}`)
  if (response.headers.get("content-type")?.startsWith("application/wasm")) {
    return WebAssembly.compileStreaming(response)
  }
  return WebAssembly.compile(await response.arrayBuffer())
}

/** Names what the module needs that the ABI does not give, or lacks that it must have. */
function checkShape(module: WebAssembly.Module, entry: string): string | null {
  for (const wanted of WebAssembly.Module.imports(module)) {
    if (
      wanted.module !== "faber_ui" ||
      wanted.kind !== "function" ||
      !(IMPORT_NAMES as readonly string[]).includes(wanted.name)
    ) {
      return `the module imports ${wanted.module}.${wanted.name}, which plugin UIs are not given`
    }
  }
  const exported = new Map(
    WebAssembly.Module.exports(module).map((item) => [item.name, item.kind]),
  )
  if (exported.get("memory") !== "memory") return "the module does not export its memory"
  // A UI that never listens or calls out needs only its entry; the callbacks
  // are required alongside the imports that lead to them.
  const imported = new Set(WebAssembly.Module.imports(module).map((item) => item.name))
  const required = ["alloc", "free", entry]
  if (imported.has("ui_on")) required.push("on_event")
  if (imported.has("rpc_call")) required.push("on_reply")
  for (const name of required) {
    if (exported.get(name) !== "function") return `the module does not export ${name}`
  }
  return null
}

async function start(url: string, { entry, context }: SurfaceEntry) {
  let module: WebAssembly.Module
  try {
    module = await compile(url)
  } catch (error) {
    fault(`the UI module did not load: ${describe(error)}`)
  }
  const missing = checkShape(module, entry)
  if (missing) fault(missing)

  // A start function runs during instantiation, so it is timed like any call.
  post({ type: "enter" })
  let instance: WebAssembly.Instance
  try {
    instance = await WebAssembly.instantiate(module, { faber_ui: imports })
  } catch (error) {
    fault(`the UI module did not start: ${describe(error)}`)
  }
  plugin = instance.exports as unknown as PluginExports
  post({ type: "exit", ops: ops.splice(0) })

  kinds.set(ROOT_HANDLE, "root")
  const run = instance.exports[entry] as (ptr: number, len: number) => void
  enter((exports) => withPayload(exports, { ...context, root: ROOT_HANDLE }, run))
}

scope.onmessage = (event: MessageEvent<ToWorker>) => {
  const message = event.data
  switch (message.type) {
    case "start":
      void start(message.url, message.entry).catch(() => {
        // `fault` already reported it.
      })
      break
    case "event": {
      // The widget may have been destroyed after the user acted on it.
      if (!listeners.has(message.listener)) return
      const { listener, payload } = message
      enter((exports) =>
        withPayload(exports, payload, (ptr, len) => exports.on_event(listener, ptr, len)),
      )
      break
    }
    case "reply":
      settle(message.id, message.reply)
      break
  }
}
