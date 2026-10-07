import * as React from "react"

import { faber, type Uuid } from "@/lib/api"
import type { RpcReply, SurfaceEntry } from "@/lib/plugin-ui/protocol"
import type { RpcTransport } from "@/lib/plugin-ui/runtime"
import { PluginSurface, type PluginSurfaceProps } from "./plugin-surface"

/** The configuration surface's id, as the RPC route names it. */
export const CONFIG_SURFACE = "config"

/**
 * The single export a plugin's `ui.wasm` provides for its configuration page.
 * It is called once with the context as JSON `(ptr, len)`:
 *
 * ```json
 * { "root": 1, "project": "<uuid>", "plugin": "environments" }
 * ```
 *
 * The plugin builds its page under `root`. The context does not carry the
 * config itself: the UI reads and writes it through its own backend
 * (`config_get` / `config_set`), which validates what it stores.
 */
export const CONFIG_ENTRY = "configure"

export interface PluginConfigProps
  extends Pick<PluginSurfaceProps, "onLog" | "height" | "className" | "fallback"> {
  project: Uuid
  /** The plugin's id, as the project's bindings name it. */
  plugin: string
  pluginName: string
  moduleUrl: string
  /** Overrides the network, for stories and tests. */
  rpc?: RpcTransport
}

/** A plugin's own configuration page for one project. */
export function PluginConfig({ project, plugin, rpc, ...props }: PluginConfigProps) {
  const entry = React.useMemo<SurfaceEntry>(
    () => ({ surface: CONFIG_SURFACE, entry: CONFIG_ENTRY, context: { project, plugin } }),
    [project, plugin],
  )
  const transport = React.useCallback<RpcTransport>(
    async (fn, args, signal) =>
      (await faber.callPluginUi(project, plugin, CONFIG_SURFACE, fn, args, signal)) as RpcReply,
    [project, plugin],
  )
  return <PluginSurface entry={entry} rpc={rpc ?? transport} {...props} />
}
