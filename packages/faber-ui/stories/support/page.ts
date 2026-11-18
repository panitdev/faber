import type { TanStackParameters } from "@storybook/tanstack-react"
import type { AnyRoute } from "@tanstack/react-router"
import type { MockApiParameters } from "@/mocks"
import "@/src/routeTree.gen"

/** Render a route inside the real app frame against the per-story mock API. */
export const at = (route: AnyRoute, extra: { params?: Record<string, string> } = {}) =>
  ({ tanstack: { router: { route, ...extra } } }) as TanStackParameters

export const fast: MockApiParameters = { latency: 0, replySpeed: 0 }
