import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, userEvent, waitFor, within } from "storybook/test"
import { IDS, db, type MockDb } from "@/mocks"
import { at } from "@/stories/support/page"
import { Route as SessionRoute } from "@/src/routes/session.$sessionId"

const meta = {
  title: "Pages/Threads/Agents",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/**
 * A thread whose agent started two others. Each spawn is a card with the
 * agent's name and live status that leads to its thread; each report is
 * shown under the agent that sent it, with how its run ended. Here both are
 * still working at first, and the cards follow them as they finish.
 */
export const WithAgents: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionAgents } }),
    mockApi: {
      seed: (store: MockDb) => {
        for (const agent of store.sessionAgents[IDS.sessionAgents] ?? []) {
          agent.status = "running"
          agent.ended_at = null
        }
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    // The cards in the transcript, not the agents panel's rows.
    const onCards = (label: string | RegExp) =>
      canvas.queryAllByText(label).filter((node) => !node.closest('[aria-label="Agents"]'))
    await waitFor(() => expect(onCards("Running")).toHaveLength(2), { timeout: 5000 })
    // Reports, under their agents' names.
    await expect(onCards(/No race: `?kick`? holds/)).toHaveLength(1)
    await expect(onCards("Finished")).toHaveLength(1)
    await expect(onCards("Failed")).toHaveLength(1)
    await expect(canvas.getByText("Message tests")).toBeVisible()

    for (const agent of db().sessionAgents[IDS.sessionAgents] ?? []) agent.status = "idle"
    await waitFor(() => expect(onCards("Done")).toHaveLength(2), { timeout: 5000 })
  },
}

/**
 * The agents panel of a thread with five agents: counts at the top, a row
 * each that follows its agent live, and stop on the ones still going.
 */
export const PanelOfFive: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionAgents } }),
    mockApi: {
      seed: (store: MockDb) => {
        const [first] = store.sessionAgents[IDS.sessionAgents] ?? []
        const now = Math.floor(Date.now() / 1000)
        store.sessionAgents[IDS.sessionAgents] = (["running", "running", "queued", "idle", "failed"] as const).map(
          (status, index) => ({
            ...first,
            id: `00000000-0000-4000-8000-0000000a9f0${index}`,
            name: `worker ${index + 1}`,
            status,
            activity: status === "running" ? "calling exec" : null,
            created_at: now - 300 + index,
            started_at: status === "queued" ? null : now - 240,
            ended_at: status === "running" || status === "queued" ? null : now - 60,
          }),
        )
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    const panel = await canvas.findByRole("region", { name: "Agents" }, { timeout: 5000 })
    const inPanel = within(panel)
    await waitFor(() => expect(panel.querySelectorAll("[data-agent-row]")).toHaveLength(5))
    await expect(inPanel.getByText("2 running")).toBeVisible()
    await expect(inPanel.getByText("2 done")).toBeVisible()
    await expect(inPanel.getByText("1 queued")).toBeVisible()

    await userEvent.click(inPanel.getByRole("button", { name: "Stop worker 1" }))
    await waitFor(() => expect(inPanel.getByText("1 running")).toBeVisible(), { timeout: 5000 })
    await expect(inPanel.getByText("3 done")).toBeVisible()
    const row = panel.querySelector('[data-agent-row="00000000-0000-4000-8000-0000000a9f00"]') as HTMLElement
    await expect(within(row).getByText("Stopped")).toBeVisible()

    // Another agent moving on shows without a reload.
    const agents = db().sessionAgents[IDS.sessionAgents] ?? []
    agents[1].activity = "calling read"
    await waitFor(() => expect(inPanel.getByText("calling read")).toBeVisible(), { timeout: 5000 })
  },
}

/** A spawned agent's own thread, with the way back to the one that started it. */
export const AgentThread: Story = {
  parameters: at(SessionRoute, { params: { sessionId: IDS.agentDiff } }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await expect(
      await canvas.findByRole("link", { name: /Back to the thread that started/ }, { timeout: 5000 }),
    ).toHaveAttribute("href", `/session/${IDS.sessionAgents}`)
  },
}
