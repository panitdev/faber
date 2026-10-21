import type { Element, ElementContent, Root } from "hast"
import ReactMarkdown, { type Components } from "react-markdown"
import remarkGfm from "remark-gfm"

import { STREAM_WORD_CLASS } from "@charlotte-ui/labs/stream-text"
import { cn } from "@charlotte-ui/utils"

/**
 * Markdown for message prose — both the agent's rows and the user's bubble.
 *
 * Every element is mapped explicitly instead of leaning on a typography preset:
 * Tailwind's preflight zeroes block margins, and the two call sites set their
 * own type scale on the container (the timeline's rows are `text-sm`, the user
 * bubble `text-[15px]`), which a `prose` class would override. Mapping keeps
 * sizes relative to whatever the caller established.
 *
 * Raw HTML is left escaped — this renders model output, so `rehype-raw` is
 * deliberately absent.
 */

const components: Components = {
  // Spacing lives on the child, not as a gap on the container, so the first and
  // last block sit flush against the caller's own padding.
  p: ({ children }) => <p className="mt-3 first:mt-0">{children}</p>,

  // Headings inherit the caller's font size, so each needs an explicit one.
  h1: ({ children }) => (
    <h1 className="mt-5 first:mt-0 text-base font-semibold tracking-tight">{children}</h1>
  ),
  h2: ({ children }) => (
    <h2 className="mt-5 first:mt-0 text-[15px] font-semibold tracking-tight">{children}</h2>
  ),
  h3: ({ children }) => (
    <h3 className="mt-4 first:mt-0 text-sm font-semibold tracking-tight">{children}</h3>
  ),
  h4: ({ children }) => (
    <h4 className="mt-4 first:mt-0 text-sm font-semibold text-muted-foreground">{children}</h4>
  ),
  h5: ({ children }) => (
    <h5 className="mt-4 first:mt-0 text-sm font-semibold text-muted-foreground">{children}</h5>
  ),
  h6: ({ children }) => (
    <h6 className="mt-4 first:mt-0 text-sm font-semibold text-muted-foreground">{children}</h6>
  ),

  ul: ({ children }) => (
    <ul className="mt-3 first:mt-0 list-disc space-y-1 pl-5 marker:text-muted-foreground">
      {children}
    </ul>
  ),
  ol: ({ children }) => (
    <ol className="mt-3 first:mt-0 list-decimal space-y-1 pl-5 marker:text-muted-foreground">
      {children}
    </ol>
  ),
  // Nested lists are children of an `li`, so their top margin would double the
  // row gap the parent list already sets.
  li: ({ children }) => <li className="[&>ul]:mt-1 [&>ol]:mt-1">{children}</li>,

  strong: ({ children }) => <strong className="font-semibold">{children}</strong>,
  em: ({ children }) => <em className="italic">{children}</em>,
  del: ({ children }) => <del className="text-muted-foreground line-through">{children}</del>,

  a: ({ children, href }) => (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer nofollow"
      className="font-medium underline decoration-border underline-offset-2 hover:decoration-current"
    >
      {children}
    </a>
  ),

  // Deliberately *not* a left rule: a 2px vertical border in `--border` is
  // exactly what AgentRun draws down the gutter, so a ruled quote reads as a
  // second timeline line. The quote is marked typographically instead — a
  // hanging glyph plus italics — which nests without stacking lines.
  blockquote: ({ children }) => (
    <blockquote className="relative mt-3 first:mt-0 pl-6 italic text-muted-foreground">
      {children}
      {/* Last in DOM order, not first: it is absolutely positioned either way,
          and a leading child would take the `first:mt-0` that the quote's own
          first paragraph needs. */}
      <span
        aria-hidden
        className="absolute left-0 top-0 select-none font-serif text-2xl not-italic leading-[0.95] text-muted-foreground/35"
      >
        &ldquo;
      </span>
    </blockquote>
  ),

  hr: () => <hr className="mt-4 first:mt-0 border-border" />,

  // `code` covers both spans and the body of a fence, and the two want opposite
  // styling. The chip is unconditional here and cancelled from inside `pre`
  // below — a fence without a language tag carries no class at all, so the
  // class is not something this side can branch on.
  code: ({ children, className }) => (
    <code className={cn("rounded bg-muted px-1 py-0.5 font-mono text-[0.9em]", className)}>
      {children}
    </code>
  ),
  // The content column is `min-w-0 flex-1`, so an unbreakable code line has to
  // scroll inside the block rather than widen the timeline.
  pre: ({ children }) => (
    <pre className="mt-3 first:mt-0 overflow-x-auto rounded-lg bg-muted p-3 text-xs leading-relaxed [&>code]:bg-transparent [&>code]:p-0 [&>code]:text-[1em]">
      {children}
    </pre>
  ),

  table: ({ children }) => (
    <div className="mt-3 first:mt-0 overflow-x-auto">
      <table className="w-full border-collapse text-left">{children}</table>
    </div>
  ),
  thead: ({ children }) => <thead className="border-b border-border">{children}</thead>,
  tr: ({ children }) => <tr className="border-b border-border last:border-0">{children}</tr>,
  th: ({ children }) => <th className="px-2 py-1.5 font-semibold">{children}</th>,
  td: ({ children }) => <td className="px-2 py-1.5 align-top">{children}</td>,

  img: ({ src, alt }) => (
    <img
      src={typeof src === "string" ? src : undefined}
      alt={alt ?? ""}
      className="mt-3 first:mt-0 max-w-full rounded-lg"
    />
  ),
}

/**
 * Splits every text node into one span per word, carrying the class
 * `StreamFade` animates on mount. React keeps the spans it already has, so as
 * the text grows only the new words fade in.
 *
 * Whitespace-only nodes are left alone: they are the gaps between block
 * elements, and inside a table a span there is invalid DOM.
 */
function rehypeStreamWords() {
  return (tree: Root) => splitWords(tree)
}

function splitWords(node: Root | Element) {
  // The root's extra child kind, a doctype, never comes out of Markdown.
  node.children = (node.children as ElementContent[]).flatMap((child): ElementContent[] => {
    if (child.type === "element") {
      splitWords(child)
      return [child]
    }
    if (child.type !== "text" || !child.value.trim()) return [child]
    return (child.value.match(/\S+\s*|\s+/g) ?? []).map(
      (word): Element => ({
        type: "element",
        tagName: "span",
        properties: { className: [STREAM_WORD_CLASS] },
        children: [{ type: "text", value: word }],
      }),
    )
  })
}

const remarkPlugins = [remarkGfm]
const streamRehypePlugins = [rehypeStreamWords]

/**
 * Renders one message's text as Markdown, inheriting the caller's type scale.
 *
 * `softBreaks` turns a single newline inside a paragraph into a visible line
 * break. Markdown says it is a space, which is right for model prose — but a
 * person typing into the prompt box means the break they typed, so the user's
 * own message opts in.
 *
 * `stream` wraps each word in its own element so `AgentRun`'s stream reveal
 * can fade it in. It only marks the words; the pacing and the fade's CSS come
 * from the row rendering this.
 */
export function Markdown({
  text,
  softBreaks = false,
  stream = false,
  className,
}: {
  text: string
  softBreaks?: boolean
  stream?: boolean
  className?: string
}) {
  return (
    <div className={cn("min-w-0 break-words", softBreaks && "[&_p]:whitespace-pre-wrap", className)}>
      <ReactMarkdown
        remarkPlugins={remarkPlugins}
        rehypePlugins={stream ? streamRehypePlugins : undefined}
        components={components}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
}
