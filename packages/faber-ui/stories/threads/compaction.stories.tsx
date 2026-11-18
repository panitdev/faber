import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, waitFor, within } from "storybook/test"
import { addSessionWithRuns, type MockApiParameters } from "@/mocks"
import { at, fast } from "@/stories/support/page"
import { Route as SessionRoute } from "@/src/routes/session.$sessionId"

const meta = {
  title: "Pages/Threads/Compaction",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

const COMPACTED_SESSION = "00000000-0000-4000-8000-00000000c0c0"
const COMPACT_ME_SESSION = "00000000-0000-4000-8000-00000000c0c1"

/**
 * A thread compacted twice: once on request (a run with no input, answered
 * with one line), and once automatically part way through a later turn, which
 * draws the divider between the rows before it and the ones after. Each
 * divider opens on the summary the model went on from.
 */
export const Compacted: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: COMPACTED_SESSION } }),
    mockApi: {
      seed: (db) => {
        addSessionWithRuns(db, {
          id: COMPACTED_SESSION,
          title: "Long refactor",
          runs: [
            {
              user: "Split `run.rs` so the launch path reads top to bottom.",
              messages: [{ text: "Done — `launch_run` now lives in `launch.rs`, and `run.rs` keeps the loop." }],
            },
            {
              user: "",
              compaction: {
                trigger: "requested",
                summarized: 6,
                summary:
                  "**Goal.** Make the run launch path readable.\n\n**Done.** `launch_run` moved to `launch.rs`.\n\n**Next.** Nothing pending.",
              },
              messages: [{ text: "Got the summary — ready for your next message." }],
            },
            {
              user: "Now run the tests and fix what breaks.",
              compaction: {
                trigger: "automatic",
                after: 1,
                tokensBefore: 163_840,
                summarized: 14,
                summary: "**Goal.** Get the suite green after the split.\n\n**State.** Two failures in `resolve.rs`, both import paths.",
              },
              messages: [
                {
                  text: "Running the suite.",
                  tools: [{ name: "exec", input: { command: "cargo test -p api" }, result: "2 failed" }],
                },
                { text: "Both failures were stale import paths; fixed, and the suite passes." },
              ],
            },
          ],
        })
      },
    } satisfies MockApiParameters,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(await canvas.findByText("Conversation compacted", {}, { timeout: 5000 })).toBeVisible()
    await expect(canvas.getByText("Conversation compacted automatically")).toBeVisible()
    await expect(canvas.getByText("164k tokens · 14 messages summarized")).toBeVisible()
    await userEvent.click(canvas.getAllByRole("button", { name: "Show the summary" })[0])
    await expect(await canvas.findByText(/launch_run/, { selector: "code" })).toBeInTheDocument()
  },
}

/** Compacts a thread from the prompt box's button, with instructions for what to keep. */
export const Compact: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: COMPACT_ME_SESSION } }),
    mockApi: {
      ...fast,
      seed: (db) => {
        addSessionWithRuns(db, {
          id: COMPACT_ME_SESSION,
          title: "Ready to compact",
          runs: [{ user: "Where does the launch path start?", messages: [{ text: "In `launch_run`, in `run.rs`." }] }],
        })
      },
    } satisfies MockApiParameters,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByText(/launch_run/, {}, { timeout: 5000 })
    await userEvent.click(canvas.getByRole("button", { name: "Compact conversation" }))
    const page = within(canvasElement.ownerDocument.body)
    await userEvent.type(await page.findByLabelText(/What to keep/), "the typecheck command")
    await userEvent.click(page.getByRole("button", { name: "Compact" }))
    await expect(
      await canvas.findByText("Conversation compacted", {}, { timeout: 8000 }),
    ).toBeInTheDocument()
    // The acknowledgement streams in word by word, one element each.
    await waitFor(() => expect(canvasElement.textContent).toMatch(/ready for your next message/), {
      timeout: 8000,
    })
  },
}
