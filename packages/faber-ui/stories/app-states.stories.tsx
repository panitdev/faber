import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect, waitFor } from "storybook/test"
import { http, HttpResponse } from "msw"
import { type MockApiParameters } from "@/mocks"
import { at } from "@/stories/support/page"
import { Route as EnvironmentsRoute } from "@/src/routes/environments"
import { Route as HomeRoute } from "@/src/routes/index"

const meta = {
  title: "Pages/App States",
  parameters: { layout: "fullscreen" },
} satisfies Meta

export default meta
type Story = StoryObj<typeof meta>

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
