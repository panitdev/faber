/**
 * Messages between the main thread and the plugin UI Worker.
 *
 * The Worker hosts one plugin module and turns its `faber_ui` import calls
 * into {@link Op}s. It flushes them as one batch each time a call into the
 * plugin returns. The main thread owns the real widgets, the watchdog and the
 * network; the Worker never fetches anything but the module itself.
 */

import type { EventName, Json } from "./widgets"

/** The handle of the root widget every surface hands its plugin. */
export const ROOT_HANDLE = 1

/** How long one call into the plugin may run before the Worker is terminated. */
export const CALL_TIMEOUT_MS = 10_000

/** Largest RPC payload, either way, in bytes of JSON. */
export const RPC_PAYLOAD_LIMIT = 1024 * 1024

/** One widget operation, in the order the plugin made it. */
export type Op =
  | { op: "create"; handle: number; kind: string }
  | { op: "destroy"; handle: number }
  | { op: "set"; handle: number; key: string; value: Json }
  | { op: "add"; parent: number; child: number; cell: Json }
  | { op: "remove"; parent: number; child: number }
  | { op: "listen"; handle: number; event: EventName; listener: number }

/**
 * What a surface asks the runtime to start: the module, the export to enter
 * it through, and the context that export is handed. The runtime adds `root`
 * to the context; everything else is the surface's own.
 */
export interface SurfaceEntry {
  /** Surface id, as the RPC route names it (`config`). */
  surface: string
  /** The export the surface calls once, with the context as JSON `(ptr, len)`. */
  entry: string
  context: { [key: string]: Json }
}

export type RpcReply =
  | { ok: Json }
  | { error: { code: string; message: string; field?: string | null } }

export type ToWorker =
  | { type: "start"; url: string; entry: SurfaceEntry }
  | { type: "event"; listener: number; payload: Json }
  | { type: "reply"; id: number; reply: RpcReply }

export type LogLevel = "error" | "warn" | "info" | "debug"

export type FromWorker =
  /** Sent just before a call into the plugin; arms the watchdog. */
  | { type: "enter" }
  /** Sent when that call returns, with every op it made. */
  | { type: "exit"; ops: Op[] }
  | { type: "rpc"; id: number; fn: string; args: Json }
  | { type: "rpc-cancel"; id: number }
  | { type: "log"; level: LogLevel; message: string }
  /** The module failed to load, trapped, or broke the ABI. The Worker is done. */
  | { type: "fault"; message: string }
