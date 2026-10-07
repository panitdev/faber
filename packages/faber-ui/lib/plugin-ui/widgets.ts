/**
 * The widget vocabulary a plugin UI may use: which kinds exist, which
 * properties and events each takes, and what values those properties accept.
 *
 * Both sides of the runtime read this. The Worker validates every `ui_set`
 * here so the plugin gets its error code synchronously; the main thread
 * validates each batch again before applying it, because the batch is applied
 * inside Faber's own page and the Worker is not trusted to have done it.
 *
 * Nothing outside this file reaches the DOM from plugin input: styles are
 * typed keys with enumerated or bounded values, colours are theme tokens, and
 * there is no way to pass a raw CSS string, a URL or HTML.
 */

export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json }

/** What a property check returns: the normalized value, or why it was refused. */
type Check = { ok: true; value: Json } | { ok: false; error: string }

type Validator = (value: Json) => Check

const ok = (value: Json): Check => ({ ok: true, value })
const fail = (error: string): Check => ({ ok: false, error })

// ---------------------------------------------------------------------------
// Value validators
// ---------------------------------------------------------------------------

const MAX_TEXT = 64 * 1024

const string: Validator = (value) =>
  typeof value === "string"
    ? value.length <= MAX_TEXT
      ? ok(value)
      : fail(`text longer than ${MAX_TEXT} characters`)
    : fail("expected a string")

const boolean: Validator = (value) =>
  typeof value === "boolean" ? ok(value) : fail("expected a boolean")

const oneOf =
  (...allowed: string[]): Validator =>
  (value) =>
    typeof value === "string" && allowed.includes(value)
      ? ok(value)
      : fail(`expected one of ${allowed.join(", ")}`)

const range =
  (min: number, max: number, integer = false): Validator =>
  (value) =>
    typeof value === "number" &&
    Number.isFinite(value) &&
    value >= min &&
    value <= max &&
    (!integer || Number.isInteger(value))
      ? ok(value)
      : fail(`expected ${integer ? "an integer" : "a number"} from ${min} to ${max}`)

const nullable =
  (inner: Validator): Validator =>
  (value) =>
    value === null ? ok(null) : inner(value)

/** Spacing in steps of 0.25rem: one number, `[y, x]`, or `[top, right, bottom, left]`. */
const SPACING_MAX = 64
const spacing: Validator = (value) => {
  const step = range(0, SPACING_MAX, true)
  if (typeof value === "number") return step(value)
  if (Array.isArray(value) && (value.length === 2 || value.length === 4)) {
    for (const part of value) {
      const checked = step(part)
      if (!checked.ok) return checked
    }
    return ok(value)
  }
  return fail(`expected a step from 0 to ${SPACING_MAX}, or an array of 2 or 4`)
}

/** A length: pixels, a percentage string, or one of the keywords. */
const size: Validator = (value) => {
  if (typeof value === "number") return range(0, 4096)(value)
  if (value === "auto" || value === "full" || value === "fit") return ok(value)
  if (typeof value === "string" && /^(100|[1-9]?\d)(\.\d+)?%$/.test(value)) return ok(value)
  return fail('expected pixels, a percentage such as "50%", or auto, full or fit')
}

/**
 * Theme tokens a plugin may colour with, mapped to Faber's CSS variables.
 * The plugin never names a colour of its own, so its UI follows the theme.
 */
export const COLOR_TOKENS = {
  background: "var(--background)",
  foreground: "var(--foreground)",
  surface: "var(--card)",
  "surface-2": "var(--muted)",
  muted: "var(--muted-foreground)",
  border: "var(--border)",
  primary: "var(--primary)",
  "primary-foreground": "var(--primary-foreground)",
  accent: "var(--accent)",
  "accent-foreground": "var(--accent-foreground)",
  danger: "var(--destructive)",
} as const

const color: Validator = oneOf(...Object.keys(COLOR_TOKENS))

const options: Validator = (value) => {
  if (!Array.isArray(value)) return fail("expected an array of { value, label }")
  if (value.length > 1000) return fail("more than 1000 options")
  for (const option of value) {
    if (
      typeof option !== "object" ||
      option === null ||
      Array.isArray(option) ||
      typeof option.value !== "string" ||
      (option.label !== undefined && typeof option.label !== "string")
    ) {
      return fail("expected an array of { value, label }")
    }
  }
  return ok(value)
}

const columns: Validator = (value) =>
  Array.isArray(value) &&
  value.length <= 32 &&
  value.every((column) => typeof column === "string")
    ? ok(value)
    : fail("expected an array of up to 32 column titles")

// ---------------------------------------------------------------------------
// Style vocabulary for primitives
// ---------------------------------------------------------------------------

const LAYOUT_STYLE: Record<string, Validator> = {
  "style.display": oneOf("flex", "grid"),
  "style.direction": oneOf("row", "column"),
  "style.columns": range(1, 12, true),
  "style.gap": range(0, SPACING_MAX, true),
  "style.align": oneOf("start", "center", "end", "stretch", "baseline"),
  "style.justify": oneOf("start", "center", "end", "between", "around", "evenly"),
  "style.wrap": boolean,
  "style.grow": range(0, 12),
  "style.basis": size,
  "style.width": size,
  "style.height": size,
  "style.min_width": size,
  "style.max_width": size,
  "style.min_height": size,
  "style.max_height": size,
  "style.padding": spacing,
  "style.margin": spacing,
  "style.overflow": oneOf("visible", "hidden", "auto"),
}

const VISUAL_STYLE: Record<string, Validator> = {
  "style.bg": color,
  "style.fg": color,
  "style.border": color,
  "style.radius": oneOf("none", "sm", "md", "lg", "xl", "full"),
  "style.shadow": oneOf("none", "sm", "md", "lg"),
  "style.opacity": range(0, 1),
}

const TEXT_STYLE: Record<string, Validator> = {
  "style.size": oneOf("xs", "sm", "base", "lg", "xl", "2xl"),
  "style.weight": oneOf("normal", "medium", "semibold", "bold"),
  "style.mono": boolean,
  "style.italic": boolean,
  "style.truncate": boolean,
  "style.align": oneOf("start", "center", "end"),
}

/** The two attributes a primitive may carry, for assistive technology. */
const ARIA: Record<string, Validator> = {
  role: oneOf("group", "region", "list", "listitem", "status", "note", "heading"),
  "aria-label": string,
}

// ---------------------------------------------------------------------------
// Kinds
// ---------------------------------------------------------------------------

export type EventName = "click" | "hover" | "key" | "change" | "submit" | "close"

export interface KindSpec {
  props: Record<string, Validator>
  events: readonly EventName[]
  /** Whether `ui_add` may put children under it. */
  container: boolean
  /** Validates the cell hints a child is added with, when the kind reads any. */
  cell?: Validator
}

const field = {
  label: string,
  disabled: boolean,
  error: nullable(string),
}

const tabCell: Validator = (value) =>
  typeof value === "object" &&
  value !== null &&
  !Array.isArray(value) &&
  typeof value.title === "string" &&
  (value.value === undefined || typeof value.value === "string")
    ? ok(value)
    : fail('a tab is added with { "title": "...", "value"?: "..." }')

const tableCell: Validator = (value) =>
  typeof value === "object" &&
  value !== null &&
  !Array.isArray(value) &&
  typeof value.row === "string" &&
  typeof value.column === "number" &&
  Number.isInteger(value.column) &&
  value.column >= 0 &&
  value.column < 32
    ? ok(value)
    : fail('a table child is added with { "row": "<key>", "column": <index> }')

export const KINDS: Record<string, KindSpec> = {
  /** The host-made root every surface hands the plugin; never created by it. */
  root: { props: {}, events: [], container: true },

  box: {
    props: { ...LAYOUT_STYLE, ...VISUAL_STYLE, ...ARIA, focusable: boolean },
    events: ["click", "hover", "key"],
    container: true,
  },
  text: {
    props: { ...TEXT_STYLE, ...VISUAL_STYLE, ...ARIA, text: string },
    events: ["click", "hover"],
    container: false,
  },

  scroll: {
    props: { "style.height": size, "style.max_height": size },
    events: [],
    container: true,
  },
  tabs: {
    props: { value: nullable(string) },
    events: ["change"],
    container: true,
    cell: tabCell,
  },
  dialog: {
    props: { open: boolean, title: string, description: string },
    events: ["close"],
    container: true,
  },

  label: { props: { text: string, muted: boolean }, events: [], container: false },
  badge: {
    props: { text: string, tone: oneOf("default", "secondary", "danger", "outline") },
    events: [],
    container: false,
  },
  alert: {
    props: {
      title: string,
      text: string,
      tone: oneOf("info", "success", "warning", "danger"),
    },
    events: [],
    container: false,
  },
  spinner: { props: { text: string }, events: [], container: false },
  divider: { props: {}, events: [], container: false },
  code: { props: { text: string }, events: [], container: false },

  button: {
    props: {
      text: string,
      disabled: boolean,
      loading: boolean,
      variant: oneOf("default", "outline", "secondary", "ghost", "danger"),
    },
    events: ["click"],
    container: false,
  },
  switch: {
    props: { ...field, checked: boolean },
    events: ["change"],
    container: false,
  },
  input: {
    props: {
      ...field,
      value: string,
      placeholder: string,
      secret: boolean,
    },
    events: ["change", "submit"],
    container: false,
  },
  textarea: {
    props: { ...field, value: string, placeholder: string, rows: range(1, 40, true) },
    events: ["change"],
    container: false,
  },
  select: {
    props: { ...field, value: nullable(string), placeholder: string, options },
    events: ["change"],
    container: false,
  },
  number: {
    props: {
      ...field,
      value: nullable(range(-Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER)),
      min: range(-Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER),
      max: range(-Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER),
      step: range(0, Number.MAX_SAFE_INTEGER),
      placeholder: string,
    },
    events: ["change"],
    container: false,
  },
  path: {
    props: { ...field, value: string, placeholder: string, environment: nullable(string) },
    events: ["change"],
    container: false,
  },

  table: {
    props: { columns, empty: string },
    events: [],
    container: true,
    cell: tableCell,
  },
  list: { props: { empty: string }, events: [], container: true },
}

/** Kinds a plugin may pass to `ui_create`. */
export function isCreatable(kind: string): boolean {
  return Object.hasOwn(KINDS, kind) && kind !== "root"
}

export type PropCheck =
  | { ok: true; value: Json }
  | { ok: false; code: "unknown_key" | "invalid_value"; error: string }

export function checkProp(kind: string, key: string, value: Json): PropCheck {
  const validate = KINDS[kind]?.props[key]
  if (!validate || !Object.hasOwn(KINDS[kind].props, key)) {
    return { ok: false, code: "unknown_key", error: `${kind} has no property "${key}"` }
  }
  const checked = validate(value)
  return checked.ok
    ? checked
    : { ok: false, code: "invalid_value", error: `${kind}.${key}: ${checked.error}` }
}

/** Validates `ui_add`'s cell hints. Kinds that read none take any, and ignore them. */
export function checkCell(kind: string, cell: Json): Check {
  const validate = KINDS[kind]?.cell
  return validate ? validate(cell) : ok(cell)
}

export function acceptsEvent(kind: string, event: string): event is EventName {
  return (KINDS[kind]?.events as readonly string[] | undefined)?.includes(event) ?? false
}
