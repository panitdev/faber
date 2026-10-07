import * as React from "react"
import { Puzzle, RotateCw, X } from "lucide-react"

import { cn } from "@/lib/utils"
import { ROOT_HANDLE, type SurfaceEntry } from "@/lib/plugin-ui/protocol"
import {
  PluginUiRuntime,
  type RpcTransport,
  type RuntimeOptions,
  type RuntimeStatus,
} from "@/lib/plugin-ui/runtime"
import { Button } from "@/components/ui/button"
import { Widget, WidgetHost } from "./widget"

export interface PluginSurfaceProps {
  /** Shown in the frame and on any dialog the plugin opens. */
  pluginName: string
  /** Where the plugin's `ui.wasm` is served. */
  moduleUrl: string
  entry: SurfaceEntry
  rpc: RpcTransport
  onLog?: RuntimeOptions["onLog"]
  /**
   * What to show once the plugin is gone: it failed, or the user aborted it.
   * `restart` loads the module again from scratch.
   */
  fallback: (status: Extract<RuntimeStatus, { state: "faulted" | "aborted" }>, restart: () => void) => React.ReactNode
  /** The frame's height. Containment needs a definite one; content scrolls inside it. */
  height?: number | string
  className?: string
}

/**
 * Hosts one plugin UI surface: starts its runtime, renders its widgets inside
 * a contained frame, and keeps an abort control the plugin cannot reach.
 *
 * Surfaces differ only in the {@link SurfaceEntry} and the RPC transport they
 * pass; see `plugin-config.tsx` for the configuration page.
 */
export function PluginSurface(props: PluginSurfaceProps) {
  const [generation, setGeneration] = React.useState(0)
  // A different module or entry is a different plugin run.
  const run = `${generation}:${props.moduleUrl}:${JSON.stringify(props.entry)}`
  return (
    <PluginSurfaceRun
      key={run}
      {...props}
      restart={() => setGeneration((value) => value + 1)}
    />
  )
}

function PluginSurfaceRun({
  pluginName,
  moduleUrl,
  entry,
  rpc,
  onLog,
  fallback,
  height = "min(70vh, 720px)",
  className,
  restart,
}: PluginSurfaceProps & { restart: () => void }) {
  // The transport and log sink may be new closures every render; the
  // runtime reads the latest ones without being rebuilt for them.
  const latest = React.useRef({ rpc, onLog })
  React.useLayoutEffect(() => {
    latest.current = { rpc, onLog }
  })

  const [runtime] = React.useState(
    () =>
      new PluginUiRuntime({
        url: moduleUrl,
        entry,
        rpc: (fn, args, signal) => latest.current.rpc(fn, args, signal),
        onLog: (level, message) =>
          latest.current.onLog
            ? latest.current.onLog(level, message)
            : console[level === "error" ? "error" : level === "warn" ? "warn" : "info"](
                `[${pluginName}] ${message}`,
              ),
      }),
  )

  React.useEffect(() => {
    runtime.start()
    return () => runtime.dispose()
  }, [runtime])

  React.useSyncExternalStore(runtime.subscribe, runtime.getVersion)
  const status = runtime.status

  if (status.state === "faulted" || status.state === "aborted") {
    return <>{fallback(status, restart)}</>
  }

  return (
    <section
      aria-label={`${pluginName} settings`}
      className={cn("flex flex-col overflow-hidden rounded-xl border border-border/60 bg-card", className)}
    >
      <header className="flex items-center gap-2 border-b border-border/60 px-3 py-2">
        <Puzzle className="size-4 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-sm font-medium">{pluginName}</span>
        <span className="text-xs text-muted-foreground">Plugin</span>
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label={`Close ${pluginName} and use the JSON editor`}
          title="Close plugin UI"
          onClick={() => runtime.abort()}
        >
          <X />
        </Button>
      </header>
      {/*
        The containment boundary: nothing inside can paint outside this box,
        whatever the style validation lets through.
      */}
      <div
        data-plugin-root
        style={{ height, contain: "strict", overflow: "hidden", isolation: "isolate" }}
      >
        <div className="h-full overflow-auto p-4">
          {status.state === "loading" ? (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
              <RotateCw className="mr-2 size-4 animate-spin" />
              Loading {pluginName}…
            </div>
          ) : (
            <WidgetHost runtime={runtime} pluginName={pluginName}>
              <Widget handle={ROOT_HANDLE} />
            </WidgetHost>
          )}
        </div>
      </div>
    </section>
  )
}
