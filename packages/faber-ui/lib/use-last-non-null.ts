import * as React from "react"

/**
 * Keeps the last non-null value around while the current one is null.
 *
 * A dialog's target is cleared the moment it starts closing, but its body
 * stays mounted for the exit animation. Reading through this keeps the
 * dialog's text identical to what it showed when it opened instead of
 * flickering to a generic fallback while it fades.
 */
export function useLastNonNull<T>(value: T | null | undefined): T | null {
  const [last, setLast] = React.useState<T | null>(value ?? null)

  React.useEffect(() => {
    if (value != null) setLast(value)
  }, [value])

  return value ?? last
}
