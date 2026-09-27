/**
 * Mock faber API for Storybook (and anything else that runs in a browser).
 *
 * `.storybook/preview.tsx` starts an MSW worker with {@link handlers} and
 * calls {@link resetMockApi} before every story with that story's
 * `parameters.mockApi`, so each story starts from known data.
 */

import { resetAgent } from "./agent"
import { resetDb, type MockApiParameters } from "./db"

export { handlers } from "./handlers"
export { db, type MockApiParameters, type MockDb } from "./db"
export { IDS, addSessionWithRuns, buildRunEvents, type CompletedRunSpec } from "./fixtures"

export function resetMockApi(parameters?: MockApiParameters) {
  resetAgent()
  return resetDb(parameters)
}
