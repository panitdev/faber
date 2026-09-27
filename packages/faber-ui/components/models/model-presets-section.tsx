"use client"

import * as React from "react"
import { ChevronDown, Layers, Pencil, Plus, Trash2 } from "lucide-react"

import type { ModelPreset, ModelPresetPricing } from "@/lib/api"
import { useModelPresets } from "@/lib/models/use-model-presets"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import {
  Table,
  TableBody,
  TableCell,
  TableEmpty,
  TableHead,
  TableHeader,
  TableLoading,
  TableRow,
  TableRowActions,
} from "@/components/ui/table"
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

      <PresetTable
        presets={owned}
        loading={!ownedLoaded}
        empty={
          <TableEmpty
            colSpan={4}
            icon={<Layers />}
            title="No presets of your own yet"
            description="Add one to describe a model you reach yourself."
          />
        }
        onEdit={openEdit}
        onDelete={setDeleteTarget}
      />

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
          {systemError ? (
            <p className="text-sm text-destructive">{systemError}</p>
          ) : (
            <PresetTable
              presets={system}
              loading={!systemLoaded && systemLoading}
              empty={<TableEmpty colSpan={3} title="No default presets" />}
            />
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

/**
 * The preset list. Actions are shown only when both handlers are given — the
 * caller's own presets are editable, the system's are not.
 */
function PresetTable({
  presets,
  loading,
  empty,
  onEdit,
  onDelete,
}: {
  presets: ModelPreset[]
  loading: boolean
  empty: React.ReactNode
  onEdit?: (preset: ModelPreset) => void
  onDelete?: (preset: ModelPreset) => void
}) {
  const editable = !!onEdit && !!onDelete
  const columns = editable ? 4 : 3
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead className="hidden sm:table-cell">Pricing</TableHead>
          <TableHead className="hidden md:table-cell">Capabilities</TableHead>
          {editable ? (
            <TableHead className="w-px">
              <span className="sr-only">Actions</span>
            </TableHead>
          ) : null}
        </TableRow>
      </TableHeader>
      <TableBody>
        {loading ? (
          <TableLoading columns={columns} />
        ) : presets.length === 0 ? (
          empty
        ) : (
          presets.map((preset) => (
            <TableRow key={preset.preset_id}>
              <TableCell className="py-2">
                <div className="flex items-center gap-2">
                  <span className="max-w-32 truncate font-medium sm:max-w-40">{preset.name}</span>
                  <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
                    {preset.provider}
                  </span>
                </div>
                <div className="max-w-44 truncate text-xs text-muted-foreground sm:max-w-56">
                  {preset.provider_name} · <span className="font-mono">{preset.id}</span>
                </div>
              </TableCell>
              <TableCell className="hidden text-muted-foreground sm:table-cell">
                {pricingLabel(preset.pricing) ?? "—"}
              </TableCell>
              <TableCell className="hidden whitespace-normal py-2 md:table-cell">
                <div className="flex flex-wrap items-center gap-1.5">
                  {capabilityChips(preset).map((chip) => (
                    <span
                      key={chip}
                      className="whitespace-nowrap rounded-md bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
                    >
                      {chip}
                    </span>
                  ))}
                </div>
              </TableCell>
              {editable ? (
                <TableCell align="end" className="w-px">
                  <TableRowActions>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Edit ${preset.name}`}
                      onClick={() => onEdit(preset)}
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Delete ${preset.name}`}
                      onClick={() => onDelete(preset)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </TableRowActions>
                </TableCell>
              ) : null}
            </TableRow>
          ))
        )}
      </TableBody>
    </Table>
  )
}
