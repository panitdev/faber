"use client"

import * as React from "react"
import { ChevronDown, X } from "lucide-react"

import {
  faber,
  FaberError,
  type CreateModelPresetRequest,
  type CreateModelProviderRequest,
  type CreatorModel,
  type ModelCost,
  type ModelOverrides,
  type ModelPreset,
  type ModelPresetProvider,
  type ModelSpec,
  type UpdateModelPresetRequest,
  type Uuid,
} from "@/lib/api"
import { cn } from "@/lib/utils"
import { AnimatedField } from "@/components/ui/animated-field"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

type Flag = "attachment" | "reasoning" | "tool_call" | "structured_output" | "temperature"

const FLAG_FIELDS: { key: Flag; label: string }[] = [
  { key: "attachment", label: "Attachments" },
  { key: "reasoning", label: "Reasoning" },
  { key: "tool_call", label: "Tool calling" },
  { key: "structured_output", label: "Structured output" },
  { key: "temperature", label: "Temperature" },
]

type CostKey =
  | "input"
  | "output"
  | "cache_read"
  | "cache_write"
  | "input_audio"
  | "output_audio"
  | "reasoning"

/** Prices held as text so a field can hold a partial number mid-edit. */
type CostForm = Record<CostKey, string>

const COST_KEYS: CostKey[] = [
  "input",
  "output",
  "cache_read",
  "cache_write",
  "input_audio",
  "output_audio",
  "reasoning",
]

const COST_LABELS: Record<CostKey, string> = {
  input: "Input",
  output: "Output",
  cache_read: "Cache read",
  cache_write: "Cache write",
  input_audio: "Input audio",
  output_audio: "Output audio",
  reasoning: "Reasoning",
}

type LimitKey = "context" | "input" | "output"
type LimitForm = Record<LimitKey, string>

const LIMIT_KEYS: LimitKey[] = ["context", "input", "output"]

const LIMIT_LABELS: Record<LimitKey, string> = {
  context: "Context window",
  input: "Max input",
  output: "Max output",
}

type OpenWeights = "unknown" | "yes" | "no"
type Status = "none" | "alpha" | "beta" | "deprecated"

/** A model's description as the form holds it: text where the user types. */
type SpecForm = {
  name: string
  description: string
  family: string
  flags: Record<Flag, boolean>
  knowledge: string
  releaseDate: string
  lastUpdated: string
  openWeights: OpenWeights
  limit: LimitForm
  modalitiesInput: string
  modalitiesOutput: string
}

type PresetFormState = {
  /** Whether the provider is picked from the caller's own or written fresh. */
  providerMode: "existing" | "new"
  providerId: string
  providerKey: string
  providerName: string
  providerDoc: string
  providerApi: string
  id: string
  spec: SpecForm
  cost: CostForm
  status: Status
}

const EMPTY_COST: CostForm = {
  input: "",
  output: "",
  cache_read: "",
  cache_write: "",
  input_audio: "",
  output_audio: "",
  reasoning: "",
}

const EMPTY_SPEC: SpecForm = {
  name: "",
  description: "",
  family: "",
  flags: {
    attachment: false,
    reasoning: false,
    tool_call: false,
    structured_output: false,
    temperature: false,
  },
  knowledge: "",
  releaseDate: "",
  lastUpdated: "",
  openWeights: "unknown",
  limit: { context: "", input: "", output: "" },
  modalitiesInput: "",
  modalitiesOutput: "",
}

const EMPTY_FORM: PresetFormState = {
  providerMode: "existing",
  providerId: "",
  providerKey: "",
  providerName: "",
  providerDoc: "",
  providerApi: "",
  id: "",
  spec: EMPTY_SPEC,
  cost: EMPTY_COST,
  status: "none",
}

/** Blank is `null` ("not stated"); a negative or non-numeric entry is invalid. */
function parsePrice(value: string): number | null | undefined {
  const trimmed = value.trim()
  if (!trimmed) return null
  const parsed = Number(trimmed)
  if (!Number.isFinite(parsed) || parsed < 0) return undefined
  return parsed
}

function parseCount(value: string): number | null | undefined {
  const trimmed = value.trim()
  if (!trimmed) return null
  const parsed = Number(trimmed)
  if (!Number.isInteger(parsed) || parsed < 0) return undefined
  return parsed
}

/** `YYYY-MM` or `YYYY-MM-DD`, the forms the catalog writes dates in. */
function isCatalogDate(value: string): boolean {
  return /^\d{4}-\d{2}(-\d{2})?$/.test(value)
}

function listFromText(value: string): string[] {
  return value
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
}

function text(value: number | string | null | undefined): string {
  return value === null || value === undefined ? "" : String(value)
}

function specForm(spec: ModelSpec): SpecForm {
  return {
    name: spec.name,
    description: text(spec.description),
    family: text(spec.family),
    flags: {
      attachment: spec.attachment,
      reasoning: spec.reasoning,
      tool_call: spec.tool_call,
      structured_output: spec.structured_output ?? false,
      temperature: spec.temperature ?? false,
    },
    knowledge: text(spec.knowledge),
    releaseDate: text(spec.release_date),
    lastUpdated: text(spec.last_updated),
    openWeights: spec.open_weights === null ? "unknown" : spec.open_weights ? "yes" : "no",
    limit: {
      context: text(spec.limit.context),
      input: text(spec.limit.input),
      output: text(spec.limit.output),
    },
    modalitiesInput: spec.modalities.input.join(", "),
    modalitiesOutput: spec.modalities.output.join(", "),
  }
}

function formFromPreset(
  preset: ModelPreset,
  providers: ModelPresetProvider[],
): PresetFormState {
  const provider = providers.find(
    (candidate) => candidate.provider_id === preset.model_provider_id,
  )
  return {
    ...EMPTY_FORM,
    providerMode: provider ? "existing" : "new",
    providerId: provider?.provider_id ?? "",
    providerKey: preset.provider,
    providerName: preset.provider_name,
    id: preset.id,
    // The resolved description, so the fields show what the preset reads as;
    // only what differs from the base model is saved back as an override.
    spec: specForm(preset),
    cost: {
      input: text(preset.cost?.input),
      output: text(preset.cost?.output),
      cache_read: text(preset.cost?.cache_read),
      cache_write: text(preset.cost?.cache_write),
      input_audio: text(preset.cost?.input_audio),
      output_audio: text(preset.cost?.output_audio),
      reasoning: text(preset.cost?.reasoning),
    },
    status: (["alpha", "beta", "deprecated"] as const).find((s) => s === preset.status) ?? "none",
  }
}

function initialForm(
  editing: ModelPreset | null,
  providers: ModelPresetProvider[],
): PresetFormState {
  if (editing) return formFromPreset(editing, providers)
  const first = providers[0]
  return {
    ...EMPTY_FORM,
    providerMode: first ? "existing" : "new",
    providerId: first?.provider_id ?? "",
  }
}

/**
 * The form's description as the catalog shape, or the first thing wrong with
 * it. A blank text field is "not stated".
 */
function specFromForm(form: SpecForm): ModelSpec | string {
  const limit = { context: null, input: null, output: null } as ModelSpec["limit"]
  for (const key of LIMIT_KEYS) {
    const parsed = parseCount(form.limit[key])
    if (parsed === undefined) return `${LIMIT_LABELS[key]} must be a non-negative whole number.`
    limit[key] = parsed
  }
  const dates = {
    knowledge: form.knowledge.trim() || null,
    release_date: form.releaseDate.trim() || null,
    last_updated: form.lastUpdated.trim() || null,
  }
  for (const date of Object.values(dates)) {
    if (date !== null && !isCatalogDate(date)) {
      return `"${date}" is not a date; use YYYY-MM or YYYY-MM-DD.`
    }
  }
  return {
    name: form.name.trim(),
    description: form.description.trim() || null,
    family: form.family.trim() || null,
    ...form.flags,
    ...dates,
    open_weights: form.openWeights === "unknown" ? null : form.openWeights === "yes",
    limit,
    modalities: {
      input: listFromText(form.modalitiesInput),
      output: listFromText(form.modalitiesOutput),
    },
  }
}

function sameJson(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

/**
 * What `spec` says differently from `base` — or, with no base, everything.
 *
 * Mirrors the server's own diff: `limit` and `modalities` are whole units, and
 * a field left blank against a base inherits the base's value, since an
 * override has no way to say "absent". A flag the base leaves unstated reads
 * as unchecked, so an untouched checkbox is not an override.
 */
function overridesOf(spec: ModelSpec, base: ModelSpec | null): Partial<ModelOverrides> {
  if (!base) return { ...spec }
  const overrides: Partial<ModelOverrides> = {}
  const differs = <K extends keyof ModelSpec>(key: K) =>
    spec[key] !== null && spec[key] !== "" && !sameJson(spec[key], base[key])
  for (const key of ["name", "description", "family", "knowledge", "release_date", "last_updated", "open_weights"] as const) {
    if (differs(key)) Object.assign(overrides, { [key]: spec[key] })
  }
  for (const key of ["attachment", "reasoning", "tool_call", "structured_output", "temperature"] as const) {
    if (spec[key] !== (base[key] ?? false)) overrides[key] = spec[key]
  }
  if (!sameJson(spec.limit, base.limit)) overrides.limit = spec.limit
  if (!sameJson(spec.modalities, base.modalities)) overrides.modalities = spec.modalities
  return overrides
}

/**
 * Searches creator models and shows the one linked. Picking one hands it to
 * `onPick`, which reseeds the description from it.
 */
function BaseModelPicker({
  value,
  onPick,
  onClear,
}: {
  value: CreatorModel | null
  onPick: (model: CreatorModel) => void
  onClear: () => void
}) {
  const [query, setQuery] = React.useState("")
  const [results, setResults] = React.useState<CreatorModel[]>([])

  React.useEffect(() => {
    const needle = query.trim()
    if (!needle) {
      setResults([])
      return
    }
    let cancelled = false
    // Debounced: every keystroke would otherwise be a request.
    const timer = setTimeout(() => {
      void faber
        .listCreatorModels({ q: needle, limit: 8 })
        .then((page) => {
          if (!cancelled) setResults(page.items)
        })
        .catch(() => {
          if (!cancelled) setResults([])
        })
    }, 200)
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [query])

  if (value) {
    return (
      <div className="flex items-center justify-between gap-2 rounded-xl border border-border px-3 py-2">
        <div className="min-w-0">
          <div className="truncate text-sm font-medium">{value.name}</div>
          <div className="truncate font-mono text-xs text-muted-foreground">{value.id}</div>
        </div>
        <Button
          type="button"
          size="icon-sm"
          variant="ghost"
          aria-label="Unlink base model"
          onClick={onClear}
        >
          <X className="h-4 w-4" />
        </Button>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-1.5">
      <AnimatedField
        id="preset-base-model"
        label="Search models"
        value={query}
        onChange={setQuery}
        placeholder="claude-opus, gpt-5, qwen3…"
      />
      {results.length > 0 ? (
        <ul className="flex flex-col overflow-hidden rounded-xl border border-border">
          {results.map((model) => (
            <li key={model.creator_model_id}>
              <button
                type="button"
                className="flex w-full items-baseline justify-between gap-3 px-3 py-2 text-left hover:bg-muted"
                onClick={() => {
                  setQuery("")
                  onPick(model)
                }}
              >
                <span className="truncate text-sm">{model.name}</span>
                <span className="shrink-0 font-mono text-xs text-muted-foreground">
                  {model.id}
                </span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}

export function ModelPresetFormDialog({
  open,
  onOpenChange,
  editing,
  providers,
  onCreate,
  onUpdate,
  onCreateProvider,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  editing: ModelPreset | null
  /** The caller's own providers — the only ones a preset may reference. */
  providers: ModelPresetProvider[]
  onCreate: (body: CreateModelPresetRequest) => Promise<ModelPreset>
  onUpdate: (id: Uuid, patch: UpdateModelPresetRequest) => Promise<ModelPreset>
  onCreateProvider: (
    body: CreateModelProviderRequest,
  ) => Promise<ModelPresetProvider>
}) {
  // The parent remounts this component (via `key`) each time the dialog opens,
  // so the lazy initializer alone is enough to seed a fresh draft.
  const [form, setForm] = React.useState<PresetFormState>(() =>
    initialForm(editing, providers),
  )
  const [base, setBase] = React.useState<CreatorModel | null>(null)
  // An edited preset's base is fetched: the response carries the resolved
  // description, not the base's own, and the diff on save needs the base.
  // The request is kept so a save made before it lands can wait for it.
  const baseRequest = React.useRef<Promise<CreatorModel | null> | null>(null)
  const editingBaseId = editing?.creator_model_id
  React.useEffect(() => {
    if (!editingBaseId) return
    let cancelled = false
    const request = faber.getCreatorModel(editingBaseId).catch(() => null)
    baseRequest.current = request
    void request.then((model) => {
      if (!cancelled) setBase((current) => current ?? model)
    })
    return () => {
      cancelled = true
    }
  }, [editingBaseId])
  const [linked, setLinked] = React.useState(!!editing?.creator_model_id)

  const [moreOpen, setMoreOpen] = React.useState(false)
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const setSpec = (patch: Partial<SpecForm>) =>
    setForm((f) => ({ ...f, spec: { ...f.spec, ...patch } }))

  const pickBase = (model: CreatorModel) => {
    setBase(model)
    setLinked(true)
    // The description starts from the model's own; what is then changed is
    // what this provider says differently.
    setForm((f) => ({ ...f, spec: specForm(model) }))
  }

  const clearBase = () => {
    // The fields keep what they show, which the preset now states itself.
    setBase(null)
    setLinked(false)
  }

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setError(null)

    const id = form.id.trim()
    if (!id) {
      setError("Model id is required.")
      return
    }
    if (!linked && !form.spec.name.trim()) {
      setError("Name is required without a base model.")
      return
    }

    if (form.providerMode === "existing" && !form.providerId) {
      setError("Pick a provider, or add a new one.")
      return
    }
    if (form.providerMode === "new") {
      if (!form.providerKey.trim() || !form.providerName.trim()) {
        setError("A new provider needs a key and a name.")
        return
      }
    }

    const spec = specFromForm(form.spec)
    if (typeof spec === "string") {
      setError(spec)
      return
    }

    // Anything the catalog wrote that the form does not show — tiered or
    // long-context rates — is kept rather than dropped by the save.
    const cost: Partial<ModelCost> = { ...(editing?.cost ?? {}) }
    for (const key of COST_KEYS) {
      const parsed = parsePrice(form.cost[key])
      if (parsed === undefined) {
        setError(`${COST_LABELS[key]} price must be a non-negative number.`)
        return
      }
      cost[key] = parsed
    }
    const stated = Object.values(cost).some((value) => value !== null && value !== undefined)

    setSubmitting(true)
    try {
      const baseModel = linked ? (base ?? (await baseRequest.current)) : null
      if (linked && !baseModel) {
        setError("Could not load the base model; unlink it or try again.")
        return
      }

      let providerId = form.providerId as Uuid
      if (form.providerMode === "new") {
        const provider = await onCreateProvider({
          id: form.providerKey.trim(),
          name: form.providerName.trim(),
          doc: form.providerDoc.trim() || null,
          api: form.providerApi.trim() || null,
        })
        providerId = provider.provider_id
      }

      const body: CreateModelPresetRequest = {
        provider_id: providerId,
        id,
        creator_model_id: baseModel?.creator_model_id ?? null,
        overrides: overridesOf(spec, baseModel),
        cost: stated ? cost : null,
        status: form.status === "none" ? null : form.status,
      }

      if (editing) await onUpdate(editing.preset_id, body)
      else await onCreate(body)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the preset")
    } finally {
      setSubmitting(false)
    }
  }

  const providerMode = form.providerMode

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85dvh] overflow-y-auto">
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{editing ? `Edit ${editing.name}` : "Add preset"}</DialogTitle>
          </DialogHeader>

          {providerMode === "existing" ? (
            <div className="w-full">
              <label
                htmlFor="preset-provider"
                className="mb-1.5 block text-sm font-medium text-foreground/80"
              >
                Provider<span className="ml-0.5 text-accent-foreground/70">*</span>
              </label>
              <Select
                value={form.providerId || undefined}
                onValueChange={(value) => setForm((f) => ({ ...f, providerId: value }))}
              >
                <SelectTrigger id="preset-provider">
                  <SelectValue placeholder="Select a provider" />
                </SelectTrigger>
                <SelectContent>
                  {providers.map((provider) => (
                    <SelectItem key={provider.provider_id} value={provider.provider_id}>
                      {provider.name} · {provider.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <button
                type="button"
                className="mt-1.5 text-sm text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
                onClick={() => setForm((f) => ({ ...f, providerMode: "new" }))}
              >
                Add a new provider instead
              </button>
            </div>
          ) : (
            <div className="flex flex-col gap-4 rounded-xl border border-border px-3 py-3">
              <div className="flex items-center justify-between gap-2">
                <span className="text-sm font-medium text-foreground/80">New provider</span>
                {providers.length > 0 ? (
                  <button
                    type="button"
                    className="text-sm text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
                    onClick={() => setForm((f) => ({ ...f, providerMode: "existing" }))}
                  >
                    Use an existing one
                  </button>
                ) : null}
              </div>
              <AnimatedField
                id="preset-provider-key"
                label="Key"
                value={form.providerKey}
                onChange={(v) => setForm((f) => ({ ...f, providerKey: v }))}
                placeholder="my-vllm"
                required
                hint="A short key for who serves the model, e.g. my-vllm."
              />
              <AnimatedField
                id="preset-provider-name"
                label="Name"
                value={form.providerName}
                onChange={(v) => setForm((f) => ({ ...f, providerName: v }))}
                placeholder="My vLLM"
                required
              />
              <AnimatedField
                id="preset-provider-doc"
                label="Documentation"
                value={form.providerDoc}
                onChange={(v) => setForm((f) => ({ ...f, providerDoc: v }))}
                placeholder="Optional"
              />
              <AnimatedField
                id="preset-provider-api"
                label="API endpoint"
                value={form.providerApi}
                onChange={(v) => setForm((f) => ({ ...f, providerApi: v }))}
                placeholder="Optional"
              />
            </div>
          )}

          <AnimatedField
            id="preset-model-id"
            label="Model id"
            value={form.id}
            onChange={(v) => setForm((f) => ({ ...f, id: v }))}
            placeholder="claude-opus-5"
            required
            hint="The id the provider serves the model under."
          />

          <div className="w-full">
            <span className="mb-1.5 block text-sm font-medium text-foreground/80">
              Base model
            </span>
            <p className="mb-3 text-sm text-muted-foreground">
              The model as its creator describes it. Linked, this preset keeps only
              what it says differently; a field left blank reads the base model&apos;s.
            </p>
            {linked && !base ? (
              <p className="text-sm text-muted-foreground">
                {editing?.base_model ?? "Loading…"}
              </p>
            ) : (
              <BaseModelPicker value={base} onPick={pickBase} onClear={clearBase} />
            )}
          </div>

          <AnimatedField
            id="preset-name"
            label="Name"
            value={form.spec.name}
            onChange={(v) => setSpec({ name: v })}
            placeholder="Claude Opus 5"
            required={!linked}
          />

          <div className="w-full">
            <span className="mb-1.5 block text-sm font-medium text-foreground/80">
              Capabilities
            </span>
            <div className="flex flex-wrap gap-x-4 gap-y-2">
              {FLAG_FIELDS.map((field) => (
                <div key={field.key} className="flex items-center gap-2">
                  <Checkbox
                    id={`preset-cap-${field.key}`}
                    checked={form.spec.flags[field.key]}
                    onCheckedChange={() =>
                      setSpec({
                        flags: {
                          ...form.spec.flags,
                          [field.key]: !form.spec.flags[field.key],
                        },
                      })
                    }
                  />
                  <label htmlFor={`preset-cap-${field.key}`} className="text-sm">
                    {field.label}
                  </label>
                </div>
              ))}
            </div>
          </div>

          <div className="w-full">
            <span className="mb-1.5 block text-sm font-medium text-foreground/80">Cost</span>
            <p className="mb-3 text-sm text-muted-foreground">
              USD per million tokens. Leave a field blank when the provider doesn&apos;t
              state it.
            </p>
            <div className="grid grid-cols-2 gap-3">
              {COST_KEYS.map((key) => (
                <AnimatedField
                  key={key}
                  id={`preset-cost-${key}`}
                  label={COST_LABELS[key]}
                  type="number"
                  value={form.cost[key]}
                  onChange={(v) => setForm((f) => ({ ...f, cost: { ...f.cost, [key]: v } }))}
                  validate={(v) =>
                    v.trim() && parsePrice(v) === undefined
                      ? "Must be a non-negative number"
                      : null
                  }
                />
              ))}
            </div>
          </div>

          <Collapsible open={moreOpen} onOpenChange={setMoreOpen}>
            <CollapsibleTrigger asChild>
              <button
                type="button"
                className="flex w-full items-center justify-between text-sm font-medium text-foreground/80"
              >
                More options
                <ChevronDown
                  className={cn("h-4 w-4 transition-transform", moreOpen && "rotate-180")}
                />
              </button>
            </CollapsibleTrigger>
            <CollapsibleContent className="flex flex-col gap-4 pt-3">
              <div className="w-full">
                <span className="mb-1.5 block text-sm font-medium text-foreground/80">
                  Limit
                </span>
                <div className="grid grid-cols-3 gap-3">
                  {LIMIT_KEYS.map((key) => (
                    <AnimatedField
                      key={key}
                      id={`preset-limit-${key}`}
                      label={LIMIT_LABELS[key]}
                      type="number"
                      value={form.spec.limit[key]}
                      onChange={(v) => setSpec({ limit: { ...form.spec.limit, [key]: v } })}
                      validate={(v) =>
                        v.trim() && parseCount(v) === undefined
                          ? "Must be a whole number"
                          : null
                      }
                    />
                  ))}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3">
                <AnimatedField
                  id="preset-modalities-input"
                  label="Input modalities"
                  value={form.spec.modalitiesInput}
                  onChange={(v) => setSpec({ modalitiesInput: v })}
                  placeholder="text, image"
                  hint="Comma-separated. image means vision."
                />
                <AnimatedField
                  id="preset-modalities-output"
                  label="Output modalities"
                  value={form.spec.modalitiesOutput}
                  onChange={(v) => setSpec({ modalitiesOutput: v })}
                  placeholder="text"
                  hint="Comma-separated."
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <AnimatedField
                  id="preset-family"
                  label="Family"
                  value={form.spec.family}
                  onChange={(v) => setSpec({ family: v })}
                  placeholder="claude-opus"
                />
                <AnimatedField
                  id="preset-description"
                  label="Description"
                  value={form.spec.description}
                  onChange={(v) => setSpec({ description: v })}
                  placeholder="Optional"
                />
              </div>

              <div className="grid grid-cols-3 gap-3">
                <AnimatedField
                  id="preset-release-date"
                  label="Release date"
                  value={form.spec.releaseDate}
                  onChange={(v) => setSpec({ releaseDate: v })}
                  placeholder="2026-03-01"
                  validate={(v) => (v.trim() && !isCatalogDate(v.trim()) ? "YYYY-MM[-DD]" : null)}
                />
                <AnimatedField
                  id="preset-last-updated"
                  label="Last updated"
                  value={form.spec.lastUpdated}
                  onChange={(v) => setSpec({ lastUpdated: v })}
                  placeholder="2026-03-01"
                  validate={(v) => (v.trim() && !isCatalogDate(v.trim()) ? "YYYY-MM[-DD]" : null)}
                />
                <AnimatedField
                  id="preset-knowledge"
                  label="Knowledge"
                  value={form.spec.knowledge}
                  onChange={(v) => setSpec({ knowledge: v })}
                  placeholder="2025-12"
                  validate={(v) => (v.trim() && !isCatalogDate(v.trim()) ? "YYYY-MM[-DD]" : null)}
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div className="w-full">
                  <label
                    htmlFor="preset-open-weights"
                    className="mb-1.5 block text-sm font-medium text-foreground/80"
                  >
                    Open weights
                  </label>
                  <Select
                    value={form.spec.openWeights}
                    onValueChange={(value) => setSpec({ openWeights: value as OpenWeights })}
                  >
                    <SelectTrigger id="preset-open-weights">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="unknown">Unstated</SelectItem>
                      <SelectItem value="yes">Yes</SelectItem>
                      <SelectItem value="no">No</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <div className="w-full">
                  <label
                    htmlFor="preset-status"
                    className="mb-1.5 block text-sm font-medium text-foreground/80"
                  >
                    Status
                  </label>
                  <Select
                    value={form.status}
                    onValueChange={(value) => setForm((f) => ({ ...f, status: value as Status }))}
                  >
                    <SelectTrigger id="preset-status">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="none">Generally available</SelectItem>
                      <SelectItem value="alpha">Alpha</SelectItem>
                      <SelectItem value="beta">Beta</SelectItem>
                      <SelectItem value="deprecated">Deprecated</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>
            </CollapsibleContent>
          </Collapsible>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button
              type="submit"
              loading={submitting}
              loadingText={editing ? "Saving" : "Adding"}
            >
              {editing ? "Save" : "Add preset"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
