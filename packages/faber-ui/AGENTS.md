<!-- BEGIN:nextjs-agent-rules -->
# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` before writing any code. Heed deprecation notices.
<!-- END:nextjs-agent-rules -->

## Component layout

`components/ui/` is registry territory — every file in it is owned by either the
`@panit` registry (`https://ui.registry.panit.dev/r/{name}.json`) or shadcn, and
`shadcn add` overwrites it wholesale. Do not hand-edit anything there. A fix that
belongs to a registry component belongs upstream in the registry.

Faber-authored components live in feature folders alongside it:

- `components/shell/` — app frame: auth wiring, the sidebar (`md` and up), the
  top bar and command drawer that replace it below `md`, profile menu
- `components/thread/` — the thread surface

Add a new folder when a feature earns one. To check whether a component is
registry-owned before touching it:

```sh
curl -s -o /dev/null -w "%{http_code}\n" https://ui.registry.panit.dev/r/<name>.json
curl -s -o /dev/null -w "%{http_code}\n" https://ui.shadcn.com/r/styles/radix-nova/<name>.json
```

## Dev server

Do not run `bun run dev` (or any dev server) unless explicitly instructed to, or you have an obvious reason that requires a live server. Prefer lint/typecheck/build for verification.

## Storybook

`bun run storybook` needs no API or Surge: `.storybook/preview.tsx` starts an
MSW worker serving the in-memory backend in `mocks/`, reset before every story.
Page-level stories live in `stories/` and render a route inside the real app
frame; component stories sit next to their component. Per story, set
`parameters.mockApi` (`scenario`, `seed`, `latency`, `replySpeed`) or override
one endpoint with `beforeEach({ msw }) { msw.use(...) }`. When the API gains a
route, add it to `mocks/handlers.ts`. `bunx vitest run --project storybook`
runs every story as a browser test.
