import type { Meta, StoryObj, TanStackParameters } from "@storybook/tanstack-react"
import type { AnyRoute } from "@tanstack/react-router"
import { http, HttpResponse } from "msw"
import { expect, userEvent, waitFor, within } from "storybook/test"

import { IDS, addSessionWithRuns, type MockApiParameters } from "@/mocks"
import "@/src/routeTree.gen"
import { Route as CredentialsRoute } from "@/src/routes/credentials"
import { Route as EnvironmentsRoute } from "@/src/routes/environments"
import { Route as HostsRoute } from "@/src/routes/hosts"
import { Route as HomeRoute } from "@/src/routes/index"
import { Route as ModelsRoute } from "@/src/routes/models"
import { Route as SessionRoute } from "@/src/routes/session.$sessionId"

/**
 * Whole pages, rendered inside the real app frame (auth gate, sidebar, top
 * bar) against the mock API in `mocks/`. Nothing here needs `crates/api` or
 * Surge running: every request is answered in the browser.
 *
 * The mock is stateful per story — create, edit, and delete all round-trip —
 * and sending a message streams a simulated reply. Say "run ls" for a tool
 * call, or include "error" for a failed run.
 *
 * Per story:
 * - `parameters.mockApi` picks starting data (`scenario`), edits it (`seed`),
 *   and sets `latency` / `replySpeed`.
 * - `beforeEach({ msw })` overrides individual endpoints.
 */
const meta = {
  title: "App",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** Renders the story at `route`, inside the app's real route tree. */
const at = (route: AnyRoute, extra: { params?: Record<string, string> } = {}) =>
  ({ tanstack: { router: { route, ...extra } } }) as TanStackParameters

const fast: MockApiParameters = { latency: 0, replySpeed: 0 }

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/** The new-thread screen. Sending here creates a session and navigates to it. */
export const Home: Story = {
  parameters: at(HomeRoute),
}

/** A finished two-turn thread: reasoning, tool calls, tables, usage. */
export const Thread: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionRefactor } }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText(/Typecheck passes/, {}, { timeout: 5000 })).toBeVisible()
  },
}

/** A run that died on a provider error, with the partial work it did. */
export const ThreadWithError: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionFlaky } }),
}

/** A session that has never had a message. */
export const EmptyThread: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionEmpty } }),
}

/**
 * A run caught mid-flight: its transcript has no terminal marker and nothing
 * is streaming it, so it stays "running" — the state to style the interrupt
 * affordance against.
 */
export const ThreadRunning: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: "00000000-0000-4000-8000-00000000abcd" } }),
    mockApi: {
      seed: (db) => {
        addSessionWithRuns(db, {
          id: "00000000-0000-4000-8000-00000000abcd",
          title: "Still thinking",
          runs: [
            {
              user: "Audit the probe code paths for timing assumptions.",
              messages: [{ thinking: "Starting with `crates/api/src/probe.rs`…", text: "Looking through the probe code now" }],
              end: null,
            },
          ],
        })
      },
    } satisfies MockApiParameters,
  },
}

/** Sends a message on an existing thread and waits for the streamed reply. */
export const SendMessage: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionEmpty } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const input = await canvas.findByRole("textbox", {}, { timeout: 5000 })
    await userEvent.type(input, "hello there{Enter}")
    // Present rather than visible: the reply is still in its reveal animation.
    await expect(
      await canvas.findByText(/comes from the Storybook mock API/, {}, { timeout: 8000 }),
    ).toBeInTheDocument()
  },
}

/** A tool call streamed live: pending → running → result. */
export const SendMessageWithTool: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionEmpty } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const input = await canvas.findByRole("textbox", {}, { timeout: 5000 })
    await userEvent.type(input, "run ls please{Enter}")
    await expect(
      await canvas.findByText(/Nothing looks out of place/, {}, { timeout: 8000 }),
    ).toBeInTheDocument()
    await expect(canvas.getByText("Run command")).toBeInTheDocument()
  },
}

// ---------------------------------------------------------------------------
// Settings pages
// ---------------------------------------------------------------------------

/** Local, SSH + docker, and agent hosts; one with a failed last probe. */
export const Hosts: Story = {
  parameters: at(HostsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText("buildbox", {}, { timeout: 5000 })).toBeVisible()
  },
}

export const Environments: Story = {
  parameters: at(EnvironmentsRoute),
}

export const Models: Story = {
  parameters: at(ModelsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findAllByText("opus", {}, { timeout: 5000 })).not.toHaveLength(0)
  },
}

export const Credentials: Story = {
  parameters: at(CredentialsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText("openrouter", {}, { timeout: 5000 })).toBeVisible()
  },
}

// ---------------------------------------------------------------------------
// Account states
// ---------------------------------------------------------------------------

/** A new account: no models, hosts, credentials, or threads yet. */
export const EmptyAccount: Story = {
  parameters: { ...at(HomeRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
}

export const EmptyHosts: Story = {
  parameters: { ...at(HostsRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
}

export const EmptyModels: Story = {
  parameters: { ...at(ModelsRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
}

/**
 * Signed out: the auth gate's inline sign-in. Any username and password work,
 * except the password "wrong".
 */
export const SignedOut: Story = {
  parameters: { ...at(HomeRoute), mockApi: { scenario: "signedOut" } satisfies MockApiParameters },
}

/** Every API call fails with a 503, as when the API is up but its pool is exhausted. */
export const ApiUnavailable: Story = {
  parameters: at(HostsRoute),
  beforeEach: ({ msw }) => {
    msw.use(
      http.all("/api/*", ({ request }) =>
        new URL(request.url).pathname.startsWith("/api/surge/")
          ? undefined
          : HttpResponse.json({ error: "database pool exhausted" }, { status: 503 }),
      ),
    )
  },
}

/** Slow network: every answer takes 2s, to see loading states. */
export const SlowNetwork: Story = {
  parameters: { ...at(HostsRoute), mockApi: { latency: 2000 } satisfies MockApiParameters },
  play: async ({ canvasElement }) => {
    await waitFor(() => expect(canvasElement.textContent?.length).toBeGreaterThan(0))
  },
}
