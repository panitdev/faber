import * as React from "react"

import { FaberError, type JsonValue } from "@/lib/api"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"
import { environmentsSettings } from "./environments-settings"

/**
 * Settings pages for the plugins this server ships, written as ordinary app
 * components. A built-in plugin and this UI are released together, so its
 * page needs neither a `ui.wasm` nor the sandbox one runs in; that runtime
 * (`components/plugin-ui/`) is for plugins Faber did not write.
 */

/** The plugin's refusals, keyed by JSON Pointer into the config; `""` is the whole config. */
export type ConfigErrors = ReadonlyMap<string, string>

export interface SettingsFormProps<Draft> {
  value: Draft
  onChange: (next: Draft) => void
  /**
   * What the plugin refused on the last save. A form shows each error by the
   * field it points at, and lists any it has no field for.
   */
  errors: ConfigErrors
  disabled: boolean
}

export interface BuiltInSettings<Draft> {
  /** The stored config as the form edits it. */
  read: (config: JsonValue) => Draft
  /** The draft as the plugin stores it. The plugin still validates it. */
  write: (draft: Draft) => JsonValue
  Form: React.ComponentType<SettingsFormProps<Draft>>
  /** Under the dialog title. */
  description: string
}

// Each page's draft type is its own; the dialog only passes a draft from
// `read` back to the same page's `Form` and `write`.
const erase = <Draft,>(settings: BuiltInSettings<Draft>) =>
  settings as unknown as BuiltInSettings<unknown>

// Keyed by plugin type id. A built-in that takes no settings (web search) has
// no entry, and gets no settings action.
const BUILT_IN: Record<string, BuiltInSettings<unknown>> = {
  environments: erase(environmentsSettings),
}

export function hasSettings(type: string): boolean {
  return Object.hasOwn(BUILT_IN, type)
}

export interface SettingsTarget {
  type: string
  name: string
  config: JsonValue
}

/**
 * Edits one binding's config. Mount it keyed on the target so the draft starts
 * from that binding's stored config.
 */
export function PluginSettingsDialog({
  target,
  onOpenChange,
  onSave,
}: {
  target: SettingsTarget | null
  onOpenChange: (open: boolean) => void
  /** Stores the config; throws the API's refusal. */
  onSave: (type: string, config: JsonValue) => Promise<void>
}) {
  const settings = target && hasSettings(target.type) ? BUILT_IN[target.type] : undefined
  return (
    <Dialog open={!!target && !!settings} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        {target && settings ? (
          <SettingsForm
            target={target}
            settings={settings}
            onSave={onSave}
            onDone={() => onOpenChange(false)}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  )
}

function SettingsForm<Draft>({
  target,
  settings,
  onSave,
  onDone,
}: {
  target: SettingsTarget
  settings: BuiltInSettings<Draft>
  onSave: (type: string, config: JsonValue) => Promise<void>
  onDone: () => void
}) {
  const [draft, setDraft] = React.useState(() => settings.read(target.config))
  const [errors, setErrors] = React.useState<ConfigErrors>(new Map())
  const [error, setError] = React.useState<string | null>(null)
  const [saving, setSaving] = React.useState(false)
  const { Form } = settings

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSaving(true)
    setError(null)
    try {
      await onSave(target.type, settings.write(draft))
      onDone()
    } catch (err) {
      const refused = err instanceof FaberError && err.code === "config-invalid"
      setErrors(refused ? configErrors(err.details) : new Map())
      setError(
        refused
          ? "Some settings need fixing."
          : err instanceof FaberError
            ? err.message
            : "the change didn't go through",
      )
    } finally {
      setSaving(false)
    }
  }

  return (
    <form onSubmit={handleSubmit} className="flex min-h-0 flex-col gap-4">
      <DialogHeader>
        <DialogTitle>{target.name}</DialogTitle>
        <DialogDescription>{settings.description}</DialogDescription>
      </DialogHeader>

      <div className="-mx-1 max-h-[60vh] overflow-y-auto px-1">
        <Form value={draft} onChange={setDraft} errors={errors} disabled={saving} />
      </div>

      {error ? <p className="text-sm text-destructive">{error}</p> : null}

      <DialogFooter>
        <Button type="submit" loading={saving} loadingText="Saving">
          Save
        </Button>
      </DialogFooter>
    </form>
  )
}

function configErrors(details: unknown): ConfigErrors {
  const found = new Map<string, string>()
  if (!Array.isArray(details)) return found
  for (const item of details) {
    if (typeof item !== "object" || item === null) continue
    const { path, message } = item as { path?: unknown; message?: unknown }
    if (typeof path !== "string" || typeof message !== "string") continue
    // One message per field is all a field shows; keep the first.
    if (!found.has(path)) found.set(path, message)
  }
  return found
}
