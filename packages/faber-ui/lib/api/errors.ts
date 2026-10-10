/**
 * The API answers every failure with `{ "error": string }` and an HTTP status
 * (`crates/api/src/error.rs`). Some refusals add a machine-readable `code`
 * (`rev-mismatch`, `name-taken`, …); otherwise branching logic keys on
 * {@link FaberError.status} — the message is a human-facing string and may
 * change.
 */
export class FaberError extends Error {
  /** HTTP status of the response, or 0 when the request never completed. */
  readonly status: number
  /** The refusal's code, when the API sent one. */
  readonly code: string | null
  /**
   * The refusal's itemized reasons, when the API sent them: for
   * `config-invalid`, the plugin's `{ path, message }[]` with `path` a JSON
   * Pointer into the config.
   */
  readonly details: unknown

  constructor(status: number, message?: string, code: string | null = null, details?: unknown) {
    super(message ?? `request failed with status ${status}`)
    this.name = "FaberError"
    this.status = status
    this.code = code
    this.details = details
  }

  /** The session is missing or expired — re-run the Surge sign-in flow. */
  get isUnauthorized(): boolean {
    return this.status === 401
  }

  get isNotFound(): boolean {
    return this.status === 404
  }

  /** 503 (pool exhausted, Surge unreachable) and 502 are worth a retry with backoff. */
  get isRetryable(): boolean {
    return this.status === 502 || this.status === 503
  }
}

/** Builds a `FaberError` from a non-2xx response, tolerating non-JSON bodies. */
export async function errorFromResponse(response: Response): Promise<FaberError> {
  let body: unknown
  try {
    body = await response.json()
  } catch {
    return new FaberError(
      response.status,
      `the API returned ${response.status} with a non-JSON body`,
    )
  }

  if (typeof body === "object" && body !== null && "error" in body) {
    const { error, code, errors } = body as { error?: unknown; code?: unknown; errors?: unknown }
    if (typeof error === "string") {
      return new FaberError(
        response.status,
        error,
        typeof code === "string" ? code : null,
        errors,
      )
    }
  }

  return new FaberError(
    response.status,
    `the API returned ${response.status} with an unrecognized error body`,
  )
}
