"use client"

import * as React from "react"

import {
  faber,
  type CreatorModel,
  type ModelCost,
  type ModelPreset,
  type ModelPresetProvider,
} from "@/lib/api"
import { AnimatedField } from "@/components/ui/animated-field"

/**
 * Pickers for the preset catalog, split in two steps.
 *
 * One search over every preset buries a model under each provider that serves
 * it — the catalog lists a popular model twenty times over. So a pick is made
 * in two short lists instead:
 *
 * - model first (`CreatorModelPicker`, then `ServingPicker`): which
 *   providers there are depends on the model, and a creator model knows every
 *   preset linked to it;
 * - provider first (`ProviderPicker`, then `PresetPicker`): the way to
 *   reach a preset no creator model describes — a local build, a router's own
 *   model — which the model-first path cannot list.
 */

/** Owned providers first, then the rest by name. */
function compareProviders(a: ModelPresetProvider, b: ModelPresetProvider): number {
  return Number(b.owned) - Number(a.owned) || a.name.localeCompare(b.name)
}

/**
 * The input-plus-dropdown both pickers share. The list opens on focus, so a
 * short list can be browsed without typing, and closes on blur — delayed, so
 * a click on an item lands before the list goes away.
 */
function SearchList<T>({
  id,
  label,
  placeholder,
  hint,
  query,
  onQueryChange,
  items,
  loading,
  empty,
  itemKey,
  renderItem,
  onPick,
}: {
  id: string
  label: string
  placeholder: string
  hint?: string
  query: string
  onQueryChange: (query: string) => void
  items: T[]
  loading?: boolean
  empty: string
  itemKey: (item: T) => string
  renderItem: (item: T) => React.ReactNode
  onPick: (item: T) => void
}) {
  const [open, setOpen] = React.useState(false)

  return (
    <div className="relative w-full">
      <AnimatedField
        id={id}
        label={label}
        value={query}
        onChange={onQueryChange}
        onFocus={() => setOpen(true)}
        onBlur={() => window.setTimeout(() => setOpen(false), 150)}
        placeholder={placeholder}
        hint={hint}
      />
      {open ? (
        <div className="absolute z-50 mt-1 max-h-64 w-full overflow-y-auto rounded-xl border border-border bg-popover text-popover-foreground shadow-lg">
          {loading && items.length === 0 ? (
            <p className="px-3 py-2 text-sm text-muted-foreground">Loading…</p>
          ) : items.length === 0 ? (
            <p className="px-3 py-2 text-sm text-muted-foreground">{empty}</p>
          ) : (
            items.map((item) => (
              <button
                key={itemKey(item)}
                type="button"
                className="flex w-full flex-col items-start gap-0.5 px-3 py-2 text-left hover:bg-muted"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => {
                  onPick(item)
                  setOpen(false)
                }}
              >
                {renderItem(item)}
              </button>
            ))
          )}
        </div>
      ) : null}
    </div>
  )
}

/** The providers the caller can see, read once per mount. */
export function useProviderList(enabled = true) {
  const [providers, setProviders] = React.useState<ModelPresetProvider[] | null>(null)

  React.useEffect(() => {
    if (!enabled) return
    let cancelled = false
    void faber
      .listModelProviders()
      .then((rows) => {
        if (!cancelled) setProviders(rows)
      })
      .catch(() => {
        if (!cancelled) setProviders([])
      })
    return () => {
      cancelled = true
    }
  }, [enabled])

  return providers
}

/**
 * Picks one provider. Filtered in the browser: a few hundred rows is one
 * request, and filtering them locally keeps typing instant.
 */
export function ProviderPicker({
  id,
  providers,
  value,
  onChange,
  hint,
}: {
  id: string
  /** `null` while the list is still loading. */
  providers: ModelPresetProvider[] | null
  value: ModelPresetProvider | null
  onChange: (provider: ModelPresetProvider | null) => void
  hint?: string
}) {
  const [query, setQuery] = React.useState("")

  const matches = React.useMemo(() => {
    const needle = query.trim().toLowerCase()
    return (providers ?? [])
      .filter(
        (provider) =>
          !needle ||
          provider.name.toLowerCase().includes(needle) ||
          provider.id.toLowerCase().includes(needle),
      )
      .sort(compareProviders)
  }, [providers, query])

  if (value) {
    return (
      <div className="w-full">
        <span className="mb-1.5 block text-sm font-medium text-foreground/80">Provider</span>
        <PickedChip
          label={value.name}
          detail={value.id}
          action="Change"
          onAction={() => {
            setQuery("")
            onChange(null)
          }}
        />
      </div>
    )
  }

  return (
    <SearchList
      id={id}
      label="Provider"
      placeholder="Search providers"
      hint={hint}
      query={query}
      onQueryChange={setQuery}
      items={matches}
      loading={providers === null}
      empty="No matching providers."
      itemKey={(provider) => provider.provider_id}
      renderItem={(provider) => (
        <>
          <span className="truncate text-sm">{provider.name}</span>
          <span className="truncate text-xs text-muted-foreground">
            {provider.id} · {provider.model_count}{" "}
            {provider.model_count === 1 ? "model" : "models"}
            {provider.owned ? " · yours" : ""}
          </span>
        </>
      )}
      onPick={(provider) => {
        setQuery("")
        onChange(provider)
      }}
    />
  )
}

/**
 * Picks one preset served by `provider`. Searched on the server, since a
 * large router serves hundreds of models; an empty query lists the first page
 * so a small provider can be browsed without typing.
 */
export function PresetPicker({
  id,
  provider,
  onPick,
  hint,
}: {
  id: string
  provider: ModelPresetProvider
  onPick: (preset: ModelPreset) => void
  hint?: string
}) {
  const [query, setQuery] = React.useState("")
  const [results, setResults] = React.useState<ModelPreset[]>([])
  const [loading, setLoading] = React.useState(true)

  React.useEffect(() => {
    let cancelled = false
    setLoading(true)
    // Debounced: every keystroke would otherwise be a request.
    const handle = window.setTimeout(() => {
      void faber
        .listModelPresets({
          model_provider_id: provider.provider_id,
          q: query.trim() || undefined,
          limit: 50,
        })
        .then((page) => {
          if (!cancelled) setResults(page.items)
        })
        .catch(() => {
          if (!cancelled) setResults([])
        })
        .finally(() => {
          if (!cancelled) setLoading(false)
        })
    }, 200)
    return () => {
      cancelled = true
      window.clearTimeout(handle)
    }
  }, [provider.provider_id, query])

  return (
    <SearchList
      id={id}
      label="Model"
      placeholder={`Search ${provider.name} models`}
      hint={hint}
      query={query}
      onQueryChange={setQuery}
      items={results}
      loading={loading}
      empty="No matching models."
      itemKey={(preset) => preset.preset_id}
      renderItem={(preset) => (
        <>
          <span className="truncate text-sm">{preset.name}</span>
          <span className="truncate font-mono text-xs text-muted-foreground">{preset.id}</span>
        </>
      )}
      onPick={(preset) => {
        setQuery("")
        onPick(preset)
      }}
    />
  )
}

/**
 * Picks one creator model. Searched on the server; an empty query lists the
 * first page, so the list can be browsed before typing.
 */
export function CreatorModelPicker({
  id,
  onPick,
  hint,
}: {
  id: string
  onPick: (model: CreatorModel) => void
  hint?: string
}) {
  const [query, setQuery] = React.useState("")
  const [results, setResults] = React.useState<CreatorModel[]>([])
  const [loading, setLoading] = React.useState(true)

  React.useEffect(() => {
    let cancelled = false
    setLoading(true)
    const handle = window.setTimeout(() => {
      void faber
        .listCreatorModels({ q: query.trim() || undefined, limit: 50 })
        .then((page) => {
          if (!cancelled) setResults(page.items)
        })
        .catch(() => {
          if (!cancelled) setResults([])
        })
        .finally(() => {
          if (!cancelled) setLoading(false)
        })
    }, 200)
    return () => {
      cancelled = true
      window.clearTimeout(handle)
    }
  }, [query])

  return (
    <SearchList
      id={id}
      label="Model"
      placeholder="Search models"
      hint={hint}
      query={query}
      onQueryChange={setQuery}
      items={results}
      loading={loading}
      empty="No matching models."
      itemKey={(model) => model.creator_model_id}
      renderItem={(model) => (
        <>
          <span className="truncate text-sm">{model.name}</span>
          <span className="truncate text-xs text-muted-foreground">
            <span className="font-mono">{model.id}</span> · served by {model.preset_count}
          </span>
        </>
      )}
      onPick={(model) => {
        setQuery("")
        onPick(model)
      }}
    />
  )
}

/** A price in the short form a picker row has room for, or `null`. */
export function costLabel(cost: ModelCost | null): string | null {
  if (!cost) return null
  const parts: string[] = []
  if (cost.input !== null) parts.push(`$${cost.input} in`)
  if (cost.output !== null) parts.push(`$${cost.output} out`)
  return parts.length > 0 ? `${parts.join(" · ")} /M` : null
}

/**
 * Picks one provider serving `model`: every preset linked to it, the caller's
 * own first. Filtered in the browser — a model is served by tens of
 * providers, not thousands.
 */
export function ServingPicker({
  id,
  model,
  onPick,
  hint,
}: {
  id: string
  model: CreatorModel
  onPick: (preset: ModelPreset) => void
  hint?: string
}) {
  const [query, setQuery] = React.useState("")
  const [presets, setPresets] = React.useState<ModelPreset[] | null>(null)

  React.useEffect(() => {
    let cancelled = false
    setPresets(null)
    void faber
      .listModelPresets({ base_model: model.id, limit: 500 })
      .then((page) => {
        if (!cancelled) setPresets(page.items)
      })
      .catch(() => {
        if (!cancelled) setPresets([])
      })
    return () => {
      cancelled = true
    }
  }, [model.id])

  const matches = React.useMemo(() => {
    const needle = query.trim().toLowerCase()
    return (presets ?? [])
      .filter(
        (preset) =>
          !needle ||
          preset.provider_name.toLowerCase().includes(needle) ||
          preset.provider.toLowerCase().includes(needle) ||
          preset.id.toLowerCase().includes(needle),
      )
      .sort(
        (a, b) =>
          Number(b.owned) - Number(a.owned) ||
          a.provider_name.localeCompare(b.provider_name) ||
          a.id.localeCompare(b.id),
      )
  }, [presets, query])

  return (
    <SearchList
      id={id}
      label="Provider"
      placeholder={`Who serves ${model.name}`}
      hint={hint}
      query={query}
      onQueryChange={setQuery}
      items={matches}
      loading={presets === null}
      empty="No provider serves this model."
      itemKey={(preset) => preset.preset_id}
      renderItem={(preset) => (
        <>
          <span className="truncate text-sm">
            {preset.provider_name}
            {preset.owned ? <span className="text-muted-foreground"> · yours</span> : null}
          </span>
          <span className="truncate text-xs text-muted-foreground">
            <span className="font-mono">{preset.id}</span>
            {costLabel(preset.cost) ? ` · ${costLabel(preset.cost)}` : ""}
            {preset.status ? ` · ${preset.status}` : ""}
          </span>
        </>
      )}
      onPick={(preset) => {
        setQuery("")
        onPick(preset)
      }}
    />
  )
}

/** A picked thing shown in place of its search, with a way back to it. */
export function PickedChip({
  label,
  detail,
  action,
  onAction,
}: {
  label: string
  detail?: string
  action: string
  onAction: () => void
}) {
  return (
    <div className="flex items-center justify-between gap-2 rounded-full border border-border bg-card px-4 py-2.5">
      <span className="min-w-0 truncate text-[15px]">
        {label}
        {detail ? (
          <span className="ml-2 font-mono text-xs text-muted-foreground">{detail}</span>
        ) : null}
      </span>
      <button
        type="button"
        className="shrink-0 text-xs text-muted-foreground hover:text-foreground"
        onClick={onAction}
      >
        {action}
      </button>
    </div>
  )
}
