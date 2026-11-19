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
 * Compact floating agents, overflow actions, and animated disclosure controls.
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
    const body = within(canvasElement.ownerDocument.body)
    const panel = await body.findByRole("region", { name: "Agents" }, { timeout: 5000 })
    const inPanel = within(panel)
    const countText = (text: string) => inPanel.getAllByText(text).find((node) => !node.closest('[aria-hidden="true"]'))!
    await waitFor(() => expect(panel.querySelectorAll("[data-agent-row]")).toHaveLength(5))
    await expect(countText("2 running")).toBeVisible()
    await expect(countText("2 done")).toBeInTheDocument()
    await expect(countText("1 queued")).toBeVisible()

    await userEvent.click(inPanel.getByRole("button", { name: "Actions for worker 1" }))
    await userEvent.click(await body.findByRole("menuitem", { name: "Stop worker 1" }))
    await waitFor(() => expect(countText("1 running")).toBeVisible(), { timeout: 5000 })
    await expect(countText("3 done")).toBeInTheDocument()
    const row = panel.querySelector('[data-agent-row="00000000-0000-4000-8000-0000000a9f00"]') as HTMLElement
    await expect(within(row).getByLabelText("stopped")).toBeInTheDocument()

    // Another agent moving on shows without a reload.
    const agents = db().sessionAgents[IDS.sessionAgents] ?? []
    agents[1].activity = "calling read"
    await userEvent.click(inPanel.getByRole("button", { name: "Actions for worker 2" }))
    await waitFor(() => expect(body.getByText("calling read")).toBeVisible(), { timeout: 5000 })
    await userEvent.keyboard("{Escape}")
    const toggle = await inPanel.findByRole("button", { name: /Agents \(5\)/ })
    await expect(toggle.clientWidth).toBe(panel.clientWidth)
    await userEvent.click(toggle)
    await expect(toggle).toHaveAttribute("aria-expanded", "false")
    await waitFor(() => expect(inPanel.queryByRole("link", { name: "Open worker 1's thread" })).not.toBeInTheDocument())
    await userEvent.click(toggle)
    await expect(toggle).toHaveAttribute("aria-expanded", "true")

    await userEvent.click(canvas.getByRole("button", { name: "Thread summary" }))
    const usage = await body.findByRole("region", { name: "Usage" })
    const sidebar = body.queryByRole("complementary", { name: "Thread summary" })
    if (sidebar) {
      const threadHeader = canvas.getByRole("button", { name: "Thread summary" }).closest("header")!
      const summaryHeader = within(sidebar).getByRole("heading", { name: "Summary" }).parentElement!
      await expect(summaryHeader.getBoundingClientRect().top).toBe(threadHeader.getBoundingClientRect().top)
      await expect(summaryHeader.getBoundingClientRect().bottom).toBe(threadHeader.getBoundingClientRect().bottom)
    }
    const usageToggle = within(usage).getByRole("button", { name: /Usage/ })
    await waitFor(() => expect(usageToggle.clientWidth).toBe(usage.clientWidth))
    const label = within(usageToggle).getAllByText("Usage").find((node) => !node.closest('[aria-hidden="true"]'))!
    const range = canvasElement.ownerDocument.createRange()
    const text = [...label.childNodes].find((node) => node.nodeType === Node.TEXT_NODE && node.textContent === "Usage")!
    range.selectNodeContents(text)
    const usageBody = within(usage).queryByRole("tablist") ?? within(usage).getByText("No usage reported yet.")
    await expect(range.getBoundingClientRect().left).toBeCloseTo(usageBody.getBoundingClientRect().left)
    const chevron = [...usageToggle.querySelectorAll("svg")].find(
      (icon) => !icon.parentElement?.closest('[aria-hidden="true"]'),
    )!
    await expect(usageToggle.getBoundingClientRect().right - chevron.getBoundingClientRect().right).toBeLessThan(30)
    await userEvent.click(usageToggle)
    await expect(usageToggle).toHaveAttribute("aria-expanded", "false")
    await expect(getComputedStyle(usage).borderRadius).toBe("23px")
    await userEvent.click(usageToggle)
    await expect(usageToggle).toHaveAttribute("aria-expanded", "true")
    await expect(getComputedStyle(usage).borderRadius).toBe("23px")
    const lens = usageToggle.querySelector('[data-slot="button-glass"]')!
    await expect(lens).toBeInTheDocument()
    const lensStyle = getComputedStyle(lens)
    await expect(lensStyle.borderBottomLeftRadius).toBe(lensStyle.borderTopLeftRadius)
    await expect(parseFloat(lensStyle.borderBottomLeftRadius)).toBeGreaterThanOrEqual(27)
    await expect(getComputedStyle(usageToggle).overflow).toBe("visible")
    await expect(getComputedStyle(usage).overflow).toBe("visible")
    await userEvent.click(await body.findByRole("button", { name: "Close summary" }))
    await waitFor(() => expect(body.queryByRole("region", { name: "Usage" })).not.toBeInTheDocument())
    await expect(panel).toBeVisible()
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

/** An empty agent list contributes no floating panel or placeholder. */
export const WithoutAgents: Story = {
  parameters: {
    ...at(SessionRoute, { params: { sessionId: IDS.sessionAgents } }),
    mockApi: { seed: (store: MockDb) => { store.sessionAgents[IDS.sessionAgents] = [] } },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement)
    await canvas.findByRole("button", { name: "Thread summary" })
    await expect(canvas.queryByRole("region", { name: "Agents" })).not.toBeInTheDocument()
    await expect(canvas.queryByText("No agents yet")).not.toBeInTheDocument()
    await userEvent.click(canvas.getByRole("button", { name: "Thread summary" }))
    const body = within(canvasElement.ownerDocument.body)
    const usage = await body.findByRole("region", { name: "Usage" })
    await waitFor(() => expect(usage).toBeVisible())
    await expect(body.queryByText("No agents yet")).not.toBeInTheDocument()
  },
}
