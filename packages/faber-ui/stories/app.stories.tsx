import type { Meta, StoryObj, TanStackParameters } from "@storybook/tanstack-react"
import type { AnyRoute } from "@tanstack/react-router"
import { http, HttpResponse } from "msw"
import { expect, fireEvent, userEvent, waitFor, within } from "storybook/test"

import { IDS, addSessionWithRuns, type MockApiParameters } from "@/mocks"
import "@/src/routeTree.gen"
import { Route as CredentialsRoute } from "@/src/routes/credentials"
import { Route as EnvironmentsRoute } from "@/src/routes/environments"
import { Route as HomeRoute } from "@/src/routes/index"
import { Route as ModelsRoute } from "@/src/routes/models"
import { Route as ProjectRoute } from "@/src/routes/project.$projectId"
import { Route as SessionRoute } from "@/src/routes/session.$sessionId"
import { Route as ThreadsRoute } from "@/src/routes/threads"

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

/** Every thread in one list, across projects and the workspace. */
export const Threads: Story = {
  parameters: at(ThreadsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(
      await canvas.findByRole("heading", { name: "Threads" }, { timeout: 5000 }),
    ).toBeVisible()
    await expect(
      (await canvas.findAllByText("Split host dialogs into their own module")).length,
    ).toBeGreaterThan(0)
  },
}

/** Selecting threads raises the action bar; deleting removes just those rows. */
export const ThreadsSelection: Story = {
  parameters: { ...at(ThreadsRoute), mockApi: fast },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByRole("heading", { name: "Threads" }, { timeout: 5000 })
    const boxes = await canvas.findAllByRole("checkbox", { name: /^Select / }, { timeout: 5000 })
    const before = boxes.length
    await expect(before).toBeGreaterThan(1)
    await expect(canvas.queryByRole("toolbar", { name: "Selected threads" })).toBeNull()

    await userEvent.click(boxes[0])
    await userEvent.click(boxes[1])
    const toolbar = await canvas.findByRole("toolbar", { name: "Selected threads" })
    await expect(within(toolbar).getByText("2 selected")).toBeVisible()

    await userEvent.click(within(toolbar).getByRole("button", { name: "Delete" }))
    const dialog = within(document.body)
    await expect(await dialog.findByText("Delete 2 threads?")).toBeVisible()
    await userEvent.click(await dialog.findByRole("button", { name: "Delete" }))

    await waitFor(
      () =>
        expect(canvas.getAllByRole("checkbox", { name: /^Select / })).toHaveLength(before - 2),
      { timeout: 5000 },
    )
    await expect(canvas.queryByRole("toolbar", { name: "Selected threads" })).toBeNull()
  },
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
// Projects
// ---------------------------------------------------------------------------

/** A project's page: its composer, its one thread, and the Environments plugin. */
export const Project: Story = {
  parameters: at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(
      await canvas.findByRole("heading", { name: "faber" }, { timeout: 5000 }),
    ).toBeVisible()
    await expect(await canvas.findByRole("switch", { name: /Disable Environments/ })).toBeVisible()
    await expect(
      canvas.getAllByText("Why is the probe test flaky?").length,
    ).toBeGreaterThan(0)
  },
}

/** Writing on a project's page starts a thread in that project and opens it. */
export const ProjectStartThread: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const input = await canvas.findByRole("textbox", {}, { timeout: 5000 })
    await userEvent.type(input, "hello there{Enter}")
    await expect(
      await canvas.findByText(/comes from the Storybook mock API/, {}, { timeout: 8000 }),
    ).toBeInTheDocument()
  },
}

/** Turning a plugin off flips the switch at once and is saved without a `rev`. */
export const ProjectDisablePlugin: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const toggle = await canvas.findByRole("switch", { name: /Disable Environments/ }, { timeout: 5000 })
    await userEvent.click(toggle)
    await waitFor(() =>
      expect(canvas.getByRole("switch", { name: /Enable Environments/ })).not.toBeChecked(),
    )
  },
}

/** Opens Environments' settings from its row menu: the built-in page, not a `ui.wasm`. */
async function openEnvironmentsSettings(canvasElement: HTMLElement) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole("button", { name: /More actions for Environments/ }, { timeout: 5000 }),
  )
  const page = within(canvasElement.ownerDocument.body)
  await userEvent.click(await page.findByRole("menuitem", { name: /Settings/ }))
  return within(await page.findByRole("dialog"))
}

/** Adding an environment picks a registered one and saves the whole list against `rev`. */
export const ProjectPluginSettings: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    const page = within(canvasElement.ownerDocument.body)
    // Stories run at a phone width: the row's name opens its editor and the
    // rest of its actions sit in its menu.
    await expect(await dialog.findByRole("button", { name: "scratch" })).toBeInTheDocument()
    await userEvent.click(dialog.getByRole("button", { name: /Add environment/ }))
    const editor = within(await page.findByRole("dialog", { name: "Add environment" }))
    await expect(editor.getByLabelText("Name")).toHaveValue("laptop")
    await userEvent.click(editor.getByRole("button", { name: "Add" }))
    await waitFor(() => expect(page.queryByRole("dialog", { name: "Add environment" })).toBeNull())
    await userEvent.click(await dialog.findByRole("button", { name: "More actions for laptop" }))
    await userEvent.click(await page.findByRole("menuitem", { name: "Move up" }))
    // The open menu hides the rest of the page from the accessibility tree.
    await waitFor(() => expect(page.queryByRole("menu")).toBeNull())
    const names = dialog
      .getAllByRole("row")
      .slice(1)
      .map((row) => row.querySelector("td:nth-child(3) button")?.textContent)
    await expect(names).toEqual(["laptop", "scratch"])
    await userEvent.click(dialog.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(page.queryByRole("dialog")).toBeNull())
  },
}

/** Dragging a row's handle reorders the list, just as the move buttons do. */
export const ProjectPluginSettingsDragToOrder: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    const page = within(canvasElement.ownerDocument.body)
    await userEvent.click(dialog.getByRole("button", { name: /Add environment/ }))
    const editor = within(await page.findByRole("dialog", { name: "Add environment" }))
    await userEvent.click(editor.getByRole("button", { name: "Add" }))
    await waitFor(() => expect(page.queryByRole("dialog", { name: "Add environment" })).toBeNull())

    const names = () =>
      dialog
        .getAllByRole("row")
        .slice(1)
        .map((row) => row.querySelector("td:nth-child(3) button")?.textContent)
    // Retried: the list stays out of the accessibility tree until the add
    // dialog's close animation finishes.
    await waitFor(() => expect(names()).toEqual(["scratch", "laptop"]))

    // Dropping on the top half of the first row puts the dragged row above it.
    const rows = dialog.getAllByRole("row").slice(1)
    const dataTransfer = new DataTransfer()
    const handle = rows[1].querySelector('[draggable="true"]')!
    fireEvent.dragStart(handle, { dataTransfer })
    fireEvent.dragOver(rows[0], { dataTransfer, clientY: rows[0].getBoundingClientRect().top + 1 })
    fireEvent.drop(rows[0], { dataTransfer })
    fireEvent.dragEnd(handle, { dataTransfer })

    await waitFor(() => expect(names()).toEqual(["laptop", "scratch"]))
    await userEvent.click(dialog.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(page.queryByRole("dialog")).toBeNull())
  },
}

/** A config the plugin refuses comes back onto the field it points at. */
export const ProjectPluginSettingsRefused: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    const page = within(canvasElement.ownerDocument.body)
    await userEvent.click(await dialog.findByRole("button", { name: "scratch" }))
    let editor = within(await page.findByRole("dialog", { name: "Configure scratch" }))
    const name = editor.getByLabelText("Name")
    await userEvent.clear(name)
    await userEvent.type(name, "a b")
    await userEvent.click(editor.getByRole("button", { name: "Apply" }))
    await userEvent.click(await dialog.findByRole("button", { name: "Save" }))
    // The refusal marks the row, and shows on the field once it is opened.
    await userEvent.click(await dialog.findByRole("button", { name: "Needs fixing a b" }))
    editor = within(await page.findByRole("dialog", { name: "Configure a b" }))
    await expect(await editor.findByText(/starting with a letter or digit/)).toBeVisible()
  },
}

/**
 * The server refuses the config with a fixed 422, as the owner check does for
 * an environment that is not the user's: the refusal shows on its field.
 * Fixed rather than computed by the mock, so the UI is checked against the
 * API's wire shape and not against the mock's copy of the rules.
 */
export const ProjectPluginSettingsRefusedByServer: Story = {
  parameters: {
    ...at(ProjectRoute, { params: { projectId: IDS.projectFaber } }),
    mockApi: fast,
  },
  beforeEach: ({ msw }) => {
    msw.use(
      http.put("/api/projects/:id/plugins/environments", () =>
        HttpResponse.json(
          {
            error: "the plugin refused this config",
            code: "config-invalid",
            errors: [
              {
                path: "/environments/0/environmentId",
                message: "not one of your registered environments",
              },
            ],
          },
          { status: 422 },
        ),
      ),
    )
  },
  play: async ({ canvasElement }) => {
    const dialog = await openEnvironmentsSettings(canvasElement)
    await userEvent.click(dialog.getByRole("button", { name: "Save" }))
    await expect(
      await dialog.findByText(/not one of your registered environments/),
    ).toBeVisible()
  },
}

// ---------------------------------------------------------------------------
// Settings pages
// ---------------------------------------------------------------------------

/** Local, SSH + docker, and agent hosts; one with a failed last probe. */
export const Environments: Story = {
  parameters: at(EnvironmentsRoute),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText("buildbox", {}, { timeout: 5000 })).toBeVisible()
  },
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

export const EmptyEnvironments: Story = {
  parameters: { ...at(EnvironmentsRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
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
  parameters: at(EnvironmentsRoute),
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
  parameters: { ...at(EnvironmentsRoute), mockApi: { latency: 2000 } satisfies MockApiParameters },
  play: async ({ canvasElement }) => {
    await waitFor(() => expect(canvasElement.textContent?.length).toBeGreaterThan(0))
  },
}
