<!-- BEGIN:nextjs-agent-rules -->
# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` before writing any code. Heed deprecation notices.
<!-- END:nextjs-agent-rules -->

## Component layout

Shared components come from Charlotte UI (`@charlotte-ui/*` packages, theme via
`@charlotte-ui/theme` in `src/globals.css`). `cn` and `DEFAULT_EASE` come from
`@charlotte-ui/utils`. A fix to a Charlotte component belongs upstream in
`panitdev/charlotte`.

`components/ui/` holds only the components Charlotte doesn't ship: copies from
the `@panit` registry (`https://ui.registry.panit.dev/r/{name}.json`) or shadcn.
Don't hand-edit them beyond pointing their imports at Charlotte. If `add` pulls
in a copy of something Charlotte ships (`button`, `responsive-dialog`,
`animated-field`, …), delete that copy and import the package instead.

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
