"use client"

import * as React from "react"

import {
  faber,
  type CreateModelPresetRequest,
  type CreateModelProviderRequest,
  type ModelPreset,
  type ModelPresetProvider,
  type UpdateModelPresetRequest,
  type Uuid,
} from "@/lib/api"

/** Rows per page of the default (system) half. */
const SYSTEM_PAGE = 50

/** Owned presets are few, so they are read in one page rather than paged. */
const OWNED_LIMIT = 500

/** The order the API lists in: provider key, then model id. */
function comparePresets(a: ModelPreset, b: ModelPreset): number {
  return a.provider.localeCompare(b.provider) || a.id.localeCompare(b.id)
}

function insertPreset(list: ModelPreset[], preset: ModelPreset): ModelPreset[] {
  return [...list, preset].sort(comparePresets)
}

/**
 * The model preset catalog, in its two halves.
 *
 * The caller's own presets are read up front — there are few of them and the
 * section renders them directly. The system half is read lazily, a page at a
 * time, because the directory publishes thousands and the section keeps it
 * collapsed until asked for. Providers come alongside so the editor can name
 * the caller's own when writing a preset.
 *
 * Every mutation returns the server's row and folds it back in, so the list is
 * the API's answer rather than a local guess.
 */
export function useModelPresets() {
  const [owned, setOwned] = React.useState<ModelPreset[]>([])
  const [ownedLoaded, setOwnedLoaded] = React.useState(false)
  const [ownedError, setOwnedError] = React.useState<string | null>(null)

  const [providers, setProviders] = React.useState<ModelPresetProvider[]>([])

  const [system, setSystem] = React.useState<ModelPreset[]>([])
  const [systemTotal, setSystemTotal] = React.useState(0)
  const [systemLoaded, setSystemLoaded] = React.useState(false)
  const [systemLoading, setSystemLoading] = React.useState(false)
  const [systemError, setSystemError] = React.useState<string | null>(null)

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listModelPresets({ owned: true, limit: OWNED_LIMIT })
      .then((page) => {
        // Re-checked client-side: an API that predates the `owned` filter
        // ignores it and answers with both halves, which would otherwise land
        // the system catalog in the editable list.
        if (!cancelled) {
          setOwned(page.items.filter((preset) => preset.owned).sort(comparePresets))
        }
      })
      .catch(() => {
        // An empty list and a failed fetch look the same on screen, so say
        // which one happened rather than showing a bare empty state.
        if (!cancelled) setOwnedError("Could not load your model presets.")
      })
      .finally(() => {
        if (!cancelled) setOwnedLoaded(true)
      })

    // A failed provider fetch is not fatal: the editor still offers a new
    // provider, and existing presets keep their row.
    void faber
      .listModelProviders()
      .then((rows) => {
        if (!cancelled) setProviders(rows)
      })
      .catch(() => {})

    return () => {
      cancelled = true
    }
  }, [])

  /** Reads one page of the system half; `offset > 0` appends. */
  const loadSystem = React.useCallback(async (offset = 0) => {
    setSystemLoading(true)
    setSystemError(null)
    try {
      const page = await faber.listModelPresets({
        owned: false,
        limit: SYSTEM_PAGE,
        offset,
      })
      setSystemTotal(page.total)
      // Same guard as the owned read, for the same version-skew reason.
      const items = page.items.filter((preset) => !preset.owned)
      setSystem((prev) => (offset === 0 ? items : [...prev, ...items]))
    } catch {
      setSystemError("Could not load the default presets.")
    } finally {
      setSystemLoaded(true)
      setSystemLoading(false)
    }
  }, [])

  const addPreset = React.useCallback(
    async (body: CreateModelPresetRequest): Promise<ModelPreset> => {
      const created = await faber.createModelPreset(body)
      setOwned((prev) => insertPreset(prev, created))
      return created
    },
    [],
  )

  const editPreset = React.useCallback(
    async (id: Uuid, patch: UpdateModelPresetRequest): Promise<ModelPreset> => {
      const updated = await faber.updateModelPreset(id, patch)
      setOwned((prev) =>
        insertPreset(
          prev.filter((preset) => preset.preset_id !== id),
          updated,
        ),
      )
      return updated
    },
    [],
  )

  const removePreset = React.useCallback(async (id: Uuid): Promise<void> => {
    await faber.deleteModelPreset(id)
    setOwned((prev) => prev.filter((preset) => preset.preset_id !== id))
  }, [])

  const addProvider = React.useCallback(
    async (body: CreateModelProviderRequest): Promise<ModelPresetProvider> => {
      const created = await faber.createModelProvider(body)
      setProviders((prev) => [...prev, created])
      return created
    },
    [],
  )

  return {
    owned,
    ownedLoaded,
    ownedError,
    providers,
    system,
    systemTotal,
    systemLoaded,
    systemLoading,
    systemError,
    loadSystem,
    addPreset,
    editPreset,
    removePreset,
    addProvider,
  }
}
