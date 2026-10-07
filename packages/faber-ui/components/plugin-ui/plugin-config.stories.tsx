import type { Meta, StoryObj } from "@storybook/tanstack-react"
import * as React from "react"
import { expect, userEvent, waitFor, within } from "storybook/test"

import type { RpcTransport } from "@/lib/plugin-ui/runtime"
import type { Json } from "@/lib/plugin-ui/widgets"
import { Button } from "@/components/ui/button"
import { PluginConfig } from "./plugin-config"
// Built from `demo/` with `cargo build --release --target wasm32-unknown-unknown`.
import demoModule from "./demo/config_demo.wasm?url"

/** The demo plugin's backend, in memory: `config_get` and `config_set`. */
function demoBackend(): RpcTransport & { saved: Json[] } {
  let config: Json = { bind_by_default: true, label: "devbox" }
  const saved: Json[] = []
  const rpc = async (fn: string, args: Json) => {
    await new Promise((resolve) => setTimeout(resolve, 50))
    if (fn === "config_get") return { ok: config }
    if (fn === "config_set") {
      config = args
      saved.push(args)
      return { ok: null }
    }
    return { error: { code: "not_allowed", message: `${fn} is not callable` } }
  }
  return Object.assign(rpc, { saved })
}

const meta = {
  component: PluginConfig,
  tags: ["ai-generated"],
  // The app mounts under StrictMode, which starts, disposes and restarts the runtime.
  decorators: [
    (Story) => (
      <React.StrictMode>
        <Story />
      </React.StrictMode>
    ),
  ],
  args: {
    project: "0199a000-0000-7000-8000-000000000001",
    plugin: "demo",
    pluginName: "Demo",
    moduleUrl: demoModule,
    height: 360,
    fallback: (status, restart) => (
      <div role="alert" className="flex flex-col items-start gap-3 rounded-xl border p-4 text-sm">
        <p>
          {status.state === "aborted"
            ? "The plugin UI was closed."
            : `The plugin UI stopped: ${status.reason}`}
        </p>
        <Button size="sm" variant="outline" onClick={restart}>
          Reload
        </Button>
      </div>
    ),
  },
} satisfies Meta<typeof PluginConfig>

export default meta
type Story = StoryObj<typeof meta>

/** Loads the config through the backend, edits it, and saves it back. */
export const EditAndSave: Story = {
  args: { rpc: demoBackend() },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement)
    await canvas.findByText("Loaded", undefined, { timeout: 5000 })
    const label = canvas.getByLabelText("Label")
    await expect(label).toHaveValue("devbox")
    await expect(canvas.getByRole("switch")).toBeChecked()

    await userEvent.click(canvas.getByRole("switch"))
    await userEvent.clear(label)
    await userEvent.type(label, "staging")
    await userEvent.click(canvas.getByRole("button", { name: "Save" }))
    await canvas.findByText("Saved")

    const saved = (args.rpc as ReturnType<typeof demoBackend>).saved
    await expect(saved.at(-1)).toEqual({ bind_by_default: false, label: "staging" })
  },
}

/** The frame's close control ends the plugin and hands over to the fallback. */
export const Abort: Story = {
  args: { rpc: demoBackend() },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByText("Loaded", undefined, { timeout: 5000 })
    await userEvent.click(canvas.getByRole("button", { name: /close demo/i }))
    await canvas.findByText("The plugin UI was closed.")
  },
}

/** A call that never returns is cut off by the watchdog after ten seconds. */
export const Hang: Story = {
  args: { rpc: demoBackend() },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByText("Loaded", undefined, { timeout: 5000 })
    await userEvent.click(canvas.getByRole("button", { name: "Hang" }))
    await waitFor(
      () => expect(canvas.getByText(/did not return within 10 seconds/)).toBeInTheDocument(),
      { timeout: 15_000 },
    )
  },
}

/** A module that is not there falls back without a frame. */
export const Missing: Story = {
  args: { rpc: demoBackend(), moduleUrl: "/plugins/demo/missing/ui.wasm" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByText(/the UI module did not load/, undefined, { timeout: 5000 })
  },
}
