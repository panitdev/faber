"use client"

import * as React from "react"
import { ChevronDown } from "lucide-react"

import {
  FaberError,
  type CreateModelPresetRequest,
  type CreateModelProviderRequest,
  type ModelPreset,
  type ModelPresetCapabilities,
  type ModelPresetPricing,
  type ModelPresetProvider,
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

const EMPTY_CAPABILITIES: ModelPresetCapabilities = {
  vision: false,
  attachment: false,
  reasoning: false,
  tools: false,
  structured_output: false,
  temperature: false,
}

const CAPABILITY_FIELDS: { key: keyof ModelPresetCapabilities; label: string }[] = [
  { key: "vision", label: "Vision" },
  { key: "attachment", label: "Attachments" },
  { key: "reasoning", label: "Reasoning" },
  { key: "tools", label: "Tool calling" },
  { key: "structured_output", label: "Structured output" },
  { key: "temperature", label: "Temperature" },
]

/** Prices held as text so a field can hold a partial number mid-edit. */
type PricingForm = {
  input: string
  output: string
  cache_read: string
  cache_write: string
  input_audio: string
  output_audio: string
  reasoning: string
}

const PRICING_KEYS: (keyof PricingForm)[] = [
  "input",
  "output",
  "cache_read",
  "cache_write",
  "input_audio",
  "output_audio",
  "reasoning",
]

const PRICING_LABELS: Record<keyof PricingForm, string> = {
  input: "Input",
  output: "Output",
  cache_read: "Cache read",
  cache_write: "Cache write",
  input_audio: "Input audio",
  output_audio: "Output audio",
  reasoning: "Reasoning",
}

type LimitsForm = { context: string; input: string; output: string }

const LIMIT_KEYS: (keyof LimitsForm)[] = ["context", "input", "output"]

const LIMIT_LABELS: Record<keyof LimitsForm, string> = {
  context: "Context window",
  input: "Max input",
  output: "Max output",
}

type OpenWeights = "unknown" | "yes" | "no"

type PresetFormState = {
  /** Whether the provider is picked from the caller's own or written fresh. */
  providerMode: "existing" | "new"
  providerId: string
  providerKey: string
  providerName: string
  providerWebsite: string
  providerApiBaseUrl: string
  id: string
  name: string
  capabilities: ModelPresetCapabilities
  pricing: PricingForm
  limits: LimitsForm
  modalitiesInput: string
  modalitiesOutput: string
  releaseDate: string
  lastUpdated: string
  knowledgeCutoff: string
  openWeights: OpenWeights
}

const EMPTY_FORM: PresetFormState = {
  providerMode: "existing",
  providerId: "",
  providerKey: "",
  providerName: "",
  providerWebsite: "",
  providerApiBaseUrl: "",
  id: "",
  name: "",
  capabilities: EMPTY_CAPABILITIES,
  pricing: {
    input: "",
    output: "",
    cache_read: "",
    cache_write: "",
    input_audio: "",
    output_audio: "",
    reasoning: "",
  },
  limits: { context: "", input: "", output: "" },
  modalitiesInput: "",
  modalitiesOutput: "",
  releaseDate: "",
  lastUpdated: "",
  knowledgeCutoff: "",
  openWeights: "unknown",
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

function epochToDate(value: number | null): string {
  if (value === null) return ""
  const date = new Date(value * 1000)
  return Number.isNaN(date.getTime()) ? "" : date.toISOString().slice(0, 10)
}

function dateToEpoch(value: string): number | null {
  const trimmed = value.trim()
  if (!trimmed) return null
  const millis = Date.parse(`${trimmed}T00:00:00Z`)
  return Number.isFinite(millis) ? Math.floor(millis / 1000) : null
}

function listFromText(value: string): string[] {
  return value
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
}

function formFromPreset(
  preset: ModelPreset,
  providers: ModelPresetProvider[],
): PresetFormState {
  const provider = providers.find((candidate) => candidate.id === preset.provider)
  return {
    providerMode: provider ? "existing" : "new",
    providerId: provider?.provider_id ?? "",
    providerKey: preset.provider,
    providerName: preset.provider_name,
    providerWebsite: "",
    providerApiBaseUrl: "",
    id: preset.id,
    name: preset.name,
    capabilities: { ...preset.capabilities },
    pricing: {
      input: text(preset.pricing.input),
      output: text(preset.pricing.output),
      cache_read: text(preset.pricing.cache_read),
      cache_write: text(preset.pricing.cache_write),
      input_audio: text(preset.pricing.input_audio),
      output_audio: text(preset.pricing.output_audio),
      reasoning: text(preset.pricing.reasoning),
    },
    limits: {
      context: text(preset.limits.context),
      input: text(preset.limits.input),
      output: text(preset.limits.output),
    },
    modalitiesInput: preset.modalities.input.join(", "),
    modalitiesOutput: preset.modalities.output.join(", "),
    releaseDate: epochToDate(preset.release_date),
    lastUpdated: epochToDate(preset.last_updated),
    knowledgeCutoff: epochToDate(preset.knowledge_cutoff),
    openWeights:
      preset.open_weights === null ? "unknown" : preset.open_weights ? "yes" : "no",
  }
}

function text(value: number | null): string {
  return value === null ? "" : String(value)
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
  const [moreOpen, setMoreOpen] = React.useState(false)
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setError(null)

    const id = form.id.trim()
    if (!id) {
      setError("Model id is required.")
      return
    }
    const name = form.name.trim()
    if (!name) {
      setError("Name is required.")
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

    const pricing = {} as ModelPresetPricing
    for (const key of PRICING_KEYS) {
      const parsed = parsePrice(form.pricing[key])
      if (parsed === undefined) {
        setError(`${PRICING_LABELS[key]} price must be a non-negative number.`)
        return
      }
      pricing[key] = parsed
    }

    const limits: Record<keyof LimitsForm, number | null> = {
      context: null,
      input: null,
      output: null,
    }
    for (const key of LIMIT_KEYS) {
      const parsed = parseCount(form.limits[key])
      if (parsed === undefined) {
        setError(`${LIMIT_LABELS[key]} must be a non-negative whole number.`)
        return
      }
      limits[key] = parsed
    }

    setSubmitting(true)
    try {
      let providerId = form.providerId as Uuid
      if (form.providerMode === "new") {
        const provider = await onCreateProvider({
          id: form.providerKey.trim(),
          name: form.providerName.trim(),
          website: form.providerWebsite.trim() || null,
          api_base_url: form.providerApiBaseUrl.trim() || null,
        })
        providerId = provider.provider_id
      }

      const body: CreateModelPresetRequest = {
        provider_id: providerId,
        id,
        name,
        capabilities: form.capabilities,
        pricing,
        limits,
        modalities: {
          input: listFromText(form.modalitiesInput),
          output: listFromText(form.modalitiesOutput),
        },
        release_date: dateToEpoch(form.releaseDate),
        last_updated: dateToEpoch(form.lastUpdated),
        knowledge_cutoff: dateToEpoch(form.knowledgeCutoff),
        open_weights:
          form.openWeights === "unknown" ? null : form.openWeights === "yes",
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
                placeholder="anthropic"
                required
                hint="The publisher's key, e.g. anthropic."
              />
              <AnimatedField
                id="preset-provider-name"
                label="Name"
                value={form.providerName}
                onChange={(v) => setForm((f) => ({ ...f, providerName: v }))}
                placeholder="Anthropic"
                required
              />
              <AnimatedField
                id="preset-provider-website"
                label="Website"
                value={form.providerWebsite}
                onChange={(v) => setForm((f) => ({ ...f, providerWebsite: v }))}
                placeholder="Optional"
              />
              <AnimatedField
                id="preset-provider-api-base-url"
                label="API base URL"
                value={form.providerApiBaseUrl}
                onChange={(v) => setForm((f) => ({ ...f, providerApiBaseUrl: v }))}
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

          <AnimatedField
            id="preset-name"
            label="Name"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="Claude Opus 5"
            required
          />

          <div className="w-full">
            <span className="mb-1.5 block text-sm font-medium text-foreground/80">
              Capabilities
            </span>
            <div className="flex flex-wrap gap-x-4 gap-y-2">
              {CAPABILITY_FIELDS.map((field) => (
                <div key={field.key} className="flex items-center gap-2">
                  <Checkbox
                    id={`preset-cap-${field.key}`}
                    checked={form.capabilities[field.key]}
                    onCheckedChange={() =>
                      setForm((f) => ({
                        ...f,
                        capabilities: {
                          ...f.capabilities,
                          [field.key]: !f.capabilities[field.key],
                        },
                      }))
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
            <span className="mb-1.5 block text-sm font-medium text-foreground/80">Pricing</span>
            <p className="mb-3 text-sm text-muted-foreground">
              USD per million tokens. Leave a field blank when the source doesn&apos;t state it.
            </p>
            <div className="grid grid-cols-2 gap-3">
              {PRICING_KEYS.map((key) => (
                <AnimatedField
                  key={key}
                  id={`preset-pricing-${key}`}
                  label={PRICING_LABELS[key]}
                  type="number"
                  value={form.pricing[key]}
                  onChange={(v) =>
                    setForm((f) => ({ ...f, pricing: { ...f.pricing, [key]: v } }))
                  }
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
                  Limits
                </span>
                <div className="grid grid-cols-3 gap-3">
                  {LIMIT_KEYS.map((key) => (
                    <AnimatedField
                      key={key}
                      id={`preset-limit-${key}`}
                      label={LIMIT_LABELS[key]}
                      type="number"
                      value={form.limits[key]}
                      onChange={(v) =>
                        setForm((f) => ({ ...f, limits: { ...f.limits, [key]: v } }))
                      }
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
                  value={form.modalitiesInput}
                  onChange={(v) => setForm((f) => ({ ...f, modalitiesInput: v }))}
                  placeholder="text, image"
                  hint="Comma-separated."
                />
                <AnimatedField
                  id="preset-modalities-output"
                  label="Output modalities"
                  value={form.modalitiesOutput}
                  onChange={(v) => setForm((f) => ({ ...f, modalitiesOutput: v }))}
                  placeholder="text"
                  hint="Comma-separated."
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <AnimatedField
                  id="preset-release-date"
                  label="Release date"
                  type="date"
                  value={form.releaseDate}
                  onChange={(v) => setForm((f) => ({ ...f, releaseDate: v }))}
                />
                <AnimatedField
                  id="preset-last-updated"
                  label="Last updated"
                  type="date"
                  value={form.lastUpdated}
                  onChange={(v) => setForm((f) => ({ ...f, lastUpdated: v }))}
                />
                <AnimatedField
                  id="preset-knowledge-cutoff"
                  label="Knowledge cutoff"
                  type="date"
                  value={form.knowledgeCutoff}
                  onChange={(v) => setForm((f) => ({ ...f, knowledgeCutoff: v }))}
                />
              </div>

              <div className="w-full">
                <label
                  htmlFor="preset-open-weights"
                  className="mb-1.5 block text-sm font-medium text-foreground/80"
                >
                  Open weights
                </label>
                <Select
                  value={form.openWeights}
                  onValueChange={(value) =>
                    setForm((f) => ({ ...f, openWeights: value as OpenWeights }))
                  }
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
