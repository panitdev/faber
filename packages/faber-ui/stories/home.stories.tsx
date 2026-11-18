import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { type MockApiParameters } from "@/mocks"
import { at } from "@/stories/support/page"
import { Route as HomeRoute } from "@/src/routes/index"

const meta = {
  title: "Pages/Home",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

/** The new-thread screen. Sending here creates a session and navigates to it. */
export const Default: Story = {
  parameters: at(HomeRoute),
}

/** A new account: no models, hosts, credentials, or threads yet. */
export const EmptyAccount: Story = {
  parameters: { ...at(HomeRoute), mockApi: { scenario: "empty" } satisfies MockApiParameters },
}
