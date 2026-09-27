import type { StorybookConfig } from '@storybook/tanstack-react';

const config: StorybookConfig = {
  "stories": [
    "../{src,components,stories}/**/*.mdx",
    "../{src,components,stories}/**/*.stories.@(js|jsx|mjs|ts|tsx)"
  ],
  "addons": [
    "@chromatic-com/storybook",
    "@storybook/addon-vitest",
    "@storybook/addon-a11y",
    "@storybook/addon-docs",
    "@storybook/addon-mcp"
  ],
  "framework": "@storybook/tanstack-react",
  // Serves `mockServiceWorker.js` (regenerate with `bunx msw init .storybook/public`).
  // Kept out of the app's own `public/` so the mock worker never ships.
  "staticDirs": ["./public"]
};
export default config;
