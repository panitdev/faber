import type { Preview } from '@storybook/tanstack-react'
import { setupWorker } from 'msw/browser'
import { createPreviewAnnotations } from 'msw-storybook-addon/preview'
import '@fontsource-variable/geist-mono'
import '../src/globals.css'

import { handlers, resetMockApi, type MockApiParameters } from '../mocks'

/**
 * Every story runs against the mock API in `mocks/` — no `crates/api`, no
 * Surge. Handlers given to `setupWorker` are the baseline and survive the
 * addon's per-story `resetHandlers()`; a story overrides one endpoint with
 * `beforeEach({ msw }) { msw.use(...) }`.
 */
const msw = createPreviewAnnotations(async () => {
  const worker = setupWorker(...handlers)
  await worker.start({ quiet: true, onUnhandledRequest: 'bypass' })
  return worker
})

const preview: Preview = {
  beforeEach: [
    msw.beforeEach!,
    (context) => {
      resetMockApi(context.parameters.mockApi as MockApiParameters | undefined)
    },
  ].flat(),

  parameters: {
    controls: {
      matchers: {
       color: /(background|color)$/i,
       date: /Date$/i,
      },
    },

    a11y: {
      // 'todo' - show a11y violations in the test UI only
      // 'error' - fail CI on a11y violations
      // 'off' - skip a11y checks entirely
      test: 'todo'
    }
  },
};

export default preview;
