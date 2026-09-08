import type { Meta, StoryObj } from "@storybook/tanstack-react"
import { expect } from "storybook/test"

import { Markdown } from "./markdown"

const meta = {
  component: Markdown,
  tags: ["ai-generated"],
} satisfies Meta<typeof Markdown>

export default meta
type Story = StoryObj<typeof meta>

export const Prose: Story = {
  args: {
    text: "# Heading\n\nSome **bold** and *italic* text, plus a [link](https://example.com).",
  },
  play: async ({ canvas }) => {
    await expect(canvas.getByRole("heading", { name: /heading/i })).toBeVisible()
    await expect(canvas.getByRole("link", { name: /link/i })).toHaveAttribute(
      "href",
      "https://example.com",
    )
  },
}

export const List: Story = {
  args: { text: "- one\n- two\n- three" },
}

export const SoftBreaks: Story = {
  args: { text: "line one\nline two", softBreaks: true },
}

// Headings use Tailwind's `font-semibold` (600) — a bare UA-styled <h1> would
// be 700 (bold), so this fails if globals.css / Tailwind did not load.
export const CssCheck: Story = {
  args: { text: "# Heading" },
  play: async ({ canvas }) => {
    const heading = canvas.getByRole("heading", { name: /heading/i })
    await expect(getComputedStyle(heading).fontWeight).toBe("600")
  },
}
