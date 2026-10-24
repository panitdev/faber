"use client"

import * as React from "react"

/**
 * What the view aims at. `data-thread-block` is on the pieces the turn renders
 * itself; `.thread-rows > *` picks up the rows of an agent run, whose markup is
 * the registry's and so carries no attribute of ours — the class goes on the
 * run's own container, which is the one thing about it we get to name.
 *
 * The run's `tail` slot wraps the indicator in a child of that container too,
 * and it is not a block: it trails whatever just arrived, and centring it would
 * push the row the user came to read above the middle. It is told apart by the
 * `data-thread-tail` the indicator carries.
 *
 * Document order is what matters, and `querySelectorAll` returns it: the last
 * match is the newest block, whichever kind it is.
 */
const BLOCK_SELECTOR = "[data-thread-block], .thread-rows > :not(:has(> [data-thread-tail]))"

/** There is no layout to measure on the server, and React says so loudly. */
const useIsoLayoutEffect = typeof window !== "undefined" ? React.useLayoutEffect : React.useEffect

/**
 * Grows an invisible tail under the transcript so the newest block can sit in
 * the MIDDLE of the view rather than at its bottom edge.
 *
 * Autoscroll still pins to the bottom — see {@link useStickToBottom} — and this
 * hook only moves where "the bottom" is. A block near the end of a thread has
 * nothing under it to scroll into view, so without a tail the only place it can
 * come to rest is the bottom edge, reading like the end of a page rather than
 * the line being written. The tail is exactly the height that shortfall costs:
 * half a viewport, less whatever already sits below the block's top.
 *
 * So the tail SHRINKS as the block grows, and once the block is half a screen
 * tall it is gone entirely — a long streaming reply fills the view normally
 * instead of dragging half a screen of blank along behind it. While it shrinks
 * by the same amount the content grows, the scroll height holds still, which is
 * what keeps the block parked at the middle without anyone re-pinning it.
 *
 * Heights are written straight to the spacer instead of held in state: this
 * runs on every resize of a streaming row, and a render per token to move one
 * `height` is a lot of React for something the DOM can be told directly. It
 * also keeps the measurement in the layout phase, before autoscroll's own
 * effect reads `scrollHeight`.
 */
function fit(el: HTMLElement | null, tail: HTMLElement | null): void {
  if (!el || !tail) return

  const blocks = el.querySelectorAll<HTMLElement>(BLOCK_SELECTOR)
  const target = blocks[blocks.length - 1]
  if (!target) {
    tail.style.height = "0px"
    return
  }

  const top = target.getBoundingClientRect().top - el.getBoundingClientRect().top + el.scrollTop
  // Everything under the block's top that is real content — the block itself,
  // the indicator, the padding that clears the prompt box. Subtracting the
  // tail's current height is what makes this idempotent: the answer does not
  // depend on the answer we gave last time.
  const below = el.scrollHeight - tail.offsetHeight - top

  tail.style.height = `${Math.max(0, Math.round(el.clientHeight / 2 - below))}px`
}

export function useCenteredTail<T extends HTMLElement>(
  container: React.RefObject<HTMLElement | null>,
  content: unknown,
): React.RefObject<T | null> {
  const spacer = React.useRef<T>(null)

  useIsoLayoutEffect(() => {
    fit(container.current, spacer.current)
  }, [container, content])

  React.useEffect(() => {
    const el = container.current
    const scrolled = el?.firstElementChild
    if (!el || !scrolled) return

    // The container for viewport changes, the content for everything else —
    // tokens filling a row, a tool result opening, a reveal animation landing.
    const observer = new ResizeObserver(() => fit(el, spacer.current))
    observer.observe(el)
    observer.observe(scrolled)

    return () => observer.disconnect()
  }, [container])

  return spacer
}
