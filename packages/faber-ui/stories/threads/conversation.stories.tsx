import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, within } from "storybook/test"
import { IDS, addSessionWithRuns, type MockApiParameters } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { pageOf, openSessionActions } from "@/stories/support/interactions"
import { Route as SessionRoute } from "@/src/routes/session.$sessionId"

const meta = {
  title: "Pages/Threads/Conversation",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** A finished two-turn thread: reasoning, tool calls, tables, usage. */
export const Default: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionRefactor } }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText(/Typecheck passes/, {}, { timeout: 5000 })).toBeVisible()
  },
}

/**
 * A thread a Discord channel talks in: its members are named, with where they
 * write from and whom they answer, and the owner's own message stays plain.
 */
export const WithOtherPeople: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionChannel } }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect((await canvas.findAllByText("Ana", {}, { timeout: 5000 })).length).toBeGreaterThan(1)
    await expect(canvas.getByText("Bo")).toBeVisible()
    await expect(canvas.getAllByText("Discord").length).toBe(3)
    await expect(canvas.getByText("Faber")).toBeVisible()
  },
}

/** A run that died on a provider error, with the partial work it did. */
export const WithError: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionFlaky } }),
}

/** A session that has never had a message. */
export const Empty: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.sessionEmpty } }),
}

/**
 * A run caught mid-flight: its transcript has no terminal marker and nothing
 * is streaming it, so it stays "running" — the state to style the interrupt
 * affordance against.
 */
export const Running: Story = {
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

/** Renaming a thread goes through the top bar's actions drawer below `md`. */
export const RenameDialog: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionRefactor } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const actions = await openSessionActions(
      canvasElement,
      "Split host dialogs into their own module",
    )
    await userEvent.click(await actions.findByRole("button", { name: "Rename" }))
    const page = pageOf(canvasElement)
    const dialog = within(await page.findByRole("dialog", { name: "Rename thread" }))
    await expect(dialog.getByLabelText("Title")).toHaveValue(
      "Split host dialogs into their own module",
    )
  },
}

/** Deleting a thread names the thread the confirm will take with it. */
export const DeleteDialog: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionRefactor } }),
    mockApi: fast,
  },
  play: async ({ canvasElement }) => {
    const actions = await openSessionActions(
      canvasElement,
      "Split host dialogs into their own module",
    )
    await userEvent.click(await actions.findByRole("button", { name: "Delete" }))
    const page = pageOf(canvasElement)
    await page.findByRole("alertdialog", { name: /Delete Split host dialogs/ })
  },
}
