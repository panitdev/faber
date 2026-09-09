import * as React from "react"

/** Tailwind's `md` — the width at which the sidebar comes back. */
export const MOBILE_BREAKPOINT = 768

/**
 * Whether the viewport is narrower than {@link MOBILE_BREAKPOINT}.
 *
 * Layout itself stays in CSS (`hidden md:flex` and friends) so the first paint
 * is already right; this is for the few decisions a media query can't make on
 * its own — closing the mobile drawer when the sidebar reappears under it.
 *
 * `useSyncExternalStore` reads the match during render rather than in an
 * effect, so there is no frame where a caller sees the wrong answer.
 */
export function useIsMobile(breakpoint: number = MOBILE_BREAKPOINT): boolean {
  const query = `(max-width: ${breakpoint - 1}px)`

  const subscribe = React.useCallback(
    (onChange: () => void) => {
      const mq = window.matchMedia(query)
      mq.addEventListener("change", onChange)
      return () => mq.removeEventListener("change", onChange)
    },
    [query],
  )

  return React.useSyncExternalStore(
    subscribe,
    () => window.matchMedia(query).matches,
    () => false,
  )
}
