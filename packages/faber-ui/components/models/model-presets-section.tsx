"use client"

import * as React from "react"
import { ChevronDown, Layers, Pencil, Plus, Trash2 } from "lucide-react"

import type { ModelPreset, ModelPresetPricing } from "@/lib/api"
import { useModelPresets } from "@/lib/models/use-model-presets"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { ModelPresetFormDialog } from "@/components/models/model-preset-dialogs"

/** How a preset's prices read, or `null` when it states none. */
function pricingLabel(pricing: ModelPresetPricing): string | null {
  const parts: string[] = []
  if (pricing.input !== null) parts.push(`$${pricing.input} in`)
  if (pricing.output !== null) parts.push(`$${pricing.output} out`)
  return parts.length > 0 ? `${parts.join(" · ")} /M` : null
}

function formatCount(value: number): string {
  if (value >= 1_000_000) return `${value / 1_000_000}m`
  if (value >= 1_000) return `${value / 1_000}k`
  return String(value)
}

/** The capability tags a preset row shows, in a fixed order. */
function capabilityChips(preset: ModelPreset): string[] {
  const chips: string[] = []
  if (preset.capabilities.vision) chips.push("vision")
  if (preset.capabilities.reasoning) chips.push("reasoning")
  if (preset.capabilities.tools) chips.push("tools")
  if (preset.capabilities.structured_output) chips.push("structured output")
  if (preset.capabilities.attachment) chips.push("attachments")
  if (preset.limits.context !== null) chips.push(`${formatCount(preset.limits.context)} ctx`)
  return chips
}

/**
 * The preset catalog, under the models it describes.
 *
 * A preset is somebody else's description of a model — what it can do and what
 * it costs — as opposed to a model definition, which is how a run reaches one.
 * The caller's own presets are editable and listed first; the system's are
 * read-only, numerous, and kept collapsed until asked for, so the page does not
 * pull the whole directory on load.
 */
export function ModelPresetsSection() {
  const {
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
  } = useModelPresets()

  const ownedProviders = React.useMemo(
    () => providers.filter((provider) => provider.owned),
    [providers],
  )

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<ModelPreset | null>(null)
  const [formKey, setFormKey] = React.useState(0)
  const [deleteTarget, setDeleteTarget] = React.useState<ModelPreset | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  const [defaultOpen, setDefaultOpen] = React.useState(false)
  // The system half is fetched on first expand, not on mount.
  const requested = React.useRef(false)

  const openCreate = () => {
    setEditing(null)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const openEdit = (preset: ModelPreset) => {
    setEditing(preset)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const handleDefaultOpenChange = (open: boolean) => {
    setDefaultOpen(open)
    if (open && !requested.current) {
      requested.current = true
      void loadSystem(0)
    }
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await removePreset(deleteTarget.preset_id)
      setDeleteTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeleting(false)
    }
  }

  return (
    <section className="flex flex-col gap-6 border-t border-border pt-10">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
        <div>
          <h2 className="text-lg font-semibold tracking-tight">Model presets</h2>
          <p className="text-sm text-muted-foreground">
            What a published model can do and what it costs. Yours are editable; the
            default catalog is read-only.
          </p>
        </div>
        <Button size="sm" className="w-full sm:w-auto" onClick={openCreate}>
          <Plus className="h-4 w-4" />
          Add preset
        </Button>
      </div>

      {ownedError ? <p className="text-sm text-destructive">{ownedError}</p> : null}

      {!ownedLoaded ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : owned.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-12 text-center">
          <Layers className="h-6 w-6 text-muted-foreground" />
          <div>
            <p className="text-sm font-medium">No presets of your own yet</p>
            <p className="text-sm text-muted-foreground">
              Add one to describe a model you reach yourself.
            </p>
          </div>
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {owned.map((preset) => (
            <li
              key={preset.preset_id}
              className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
            >
              <PresetDetails preset={preset} />
              <div className="flex shrink-0 items-center gap-1">
                <Button
                  size="icon-sm"
                  variant="ghost"
                  aria-label={`Edit ${preset.name}`}
                  onClick={() => openEdit(preset)}
                >
                  <Pencil className="h-4 w-4" />
                </Button>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  aria-label={`Delete ${preset.name}`}
                  onClick={() => setDeleteTarget(preset)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <Collapsible open={defaultOpen} onOpenChange={handleDefaultOpenChange}>
        <CollapsibleTrigger asChild>
          <button
            type="button"
            className="flex w-full items-center justify-between rounded-lg py-1 text-sm font-medium text-foreground/80"
          >
            <span>
              Default presets
              {systemLoaded ? (
                <span className="ml-2 font-normal text-muted-foreground">{systemTotal}</span>
              ) : null}
            </span>
            <ChevronDown
              className={cn("h-4 w-4 transition-transform", defaultOpen && "rotate-180")}
            />
          </button>
        </CollapsibleTrigger>
        <CollapsibleContent className="pt-3">
          {!systemLoaded && systemLoading ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : systemError ? (
            <p className="text-sm text-destructive">{systemError}</p>
          ) : system.length === 0 ? (
            <p className="text-sm text-muted-foreground">No default presets.</p>
          ) : (
            <ul className="flex flex-col gap-2">
              {system.map((preset) => (
                <li
                  key={preset.preset_id}
                  className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
                >
                  <PresetDetails preset={preset} />
                </li>
              ))}
            </ul>
          )}

          {systemLoaded && system.length < systemTotal ? (
            <div className="mt-3 flex justify-center">
              <Button
                size="sm"
                variant="ghost"
                loading={systemLoading}
                loadingText="Loading"
                onClick={() => void loadSystem(system.length)}
              >
                Load more
              </Button>
            </div>
          ) : null}
        </CollapsibleContent>
      </Collapsible>

      <ModelPresetFormDialog
        key={formKey}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        editing={editing}
        providers={ownedProviders}
        onCreate={addPreset}
        onUpdate={editPreset}
        onCreateProvider={addProvider}
      />

      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Model definitions already using this preset keep working — nothing routes
              through a preset. This can&apos;t be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void handleDelete()
              }}
              disabled={deleting}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {deleting ? "Deleting…" : "Delete"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}

function PresetDetails({ preset }: { preset: ModelPreset }) {
  const chips = capabilityChips(preset)
  const price = pricingLabel(preset.pricing)
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        <span className="truncate text-sm font-medium">{preset.name}</span>
        <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
          {preset.provider}
        </span>
      </div>
      <p className="truncate text-xs text-muted-foreground">
        {preset.provider_name} · {preset.id}
        {price ? ` · ${price}` : ""}
      </p>
      {chips.length > 0 ? (
        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
          {chips.map((chip) => (
            <span
              key={chip}
              className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
            >
              {chip}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  )
}
