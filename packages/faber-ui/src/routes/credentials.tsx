import * as React from "react"
import { createFileRoute } from "@tanstack/react-router"
import { Plus, Trash2 } from "lucide-react"

import { faber, FaberError, type Credential, type CredentialKind } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { AnimatedField } from "@/components/ui/animated-field"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
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
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

function formatDate(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  })
}

export const Route = createFileRoute("/credentials")({ component: CredentialsPage })

function CredentialsPage() {
  const [credentials, setCredentials] = React.useState<Credential[]>([])
  const [loaded, setLoaded] = React.useState(false)

  React.useEffect(() => {
    let cancelled = false
    void faber
      .listCredentials()
      .then((rows) => {
        if (!cancelled) setCredentials(rows)
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoaded(true)
      })
    return () => {
      cancelled = true
    }
  }, [])

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [formKey, setFormKey] = React.useState(0)

  const openCreate = () => {
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const addCredential = React.useCallback(
    async (label: string, kind: CredentialKind, key: string): Promise<Credential> => {
      const created = await faber.createCredential({ label, kind, key })
      setCredentials((prev) => [...prev, created])
      return created
    },
    [],
  )

  const [deleteTarget, setDeleteTarget] = React.useState<Credential | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await faber.deleteCredential(deleteTarget.id)
      setCredentials((prev) => prev.filter((c) => c.id !== deleteTarget.id))
      setDeleteTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeleting(false)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 px-4 py-6 md:py-10">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Credentials</h1>
            <p className="text-sm text-muted-foreground">
              Encrypted secrets used by models and SSH hosts. Only the last four characters ever
              come back.
            </p>
          </div>
          <Button
            size="sm"
            className="w-full sm:w-auto"
            onClick={openCreate}
          >
            <Plus className="h-4 w-4" />
            Add credential
          </Button>
        </div>

        {!loaded ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : (
          <div className="flex flex-col gap-8">
            <CredentialSection
              title="API keys"
              description="Provider credentials used by models."
              credentials={credentials.filter((credential) => credential.kind === "api_key")}
              onDelete={setDeleteTarget}
            />
            <CredentialSection
              title="SSH keys"
              description="Private keys used to connect to remote hosts."
              credentials={credentials.filter((credential) => credential.kind === "ssh_key")}
              onDelete={setDeleteTarget}
            />
          </div>
        )}
      </div>

      <CredentialFormDialog
        key={formKey}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        onCreate={addCredential}
      />

      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.label}?</AlertDialogTitle>
            <AlertDialogDescription>
              Any model still using this credential will fail to authenticate. This can&apos;t be
              undone.
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
    </div>
  )
}

function CredentialSection({
  title,
  description,
  credentials,
  onDelete,
}: {
  title: string
  description: string
  credentials: Credential[]
  onDelete: (credential: Credential) => void
}) {
  return (
    <section className="flex flex-col gap-3">
      <div>
        <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      {credentials.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border px-3 py-4 text-sm text-muted-foreground">
          None added.
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {credentials.map((credential) => (
            <li
              key={credential.id}
              className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
            >
              <div className="min-w-0">
                <span className="truncate text-sm font-medium">{credential.label}</span>
                <p className="truncate text-xs text-muted-foreground">
                  ····{credential.last_four} · added {formatDate(credential.created_at)}
                </p>
              </div>
              <Button
                size="icon-sm"
                variant="ghost"
                aria-label={`Delete ${credential.label}`}
                onClick={() => onDelete(credential)}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function CredentialFormDialog({
  open,
  onOpenChange,
  onCreate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreate: (label: string, kind: CredentialKind, key: string) => Promise<Credential>
}) {
  const [label, setLabel] = React.useState("")
  const [kind, setKind] = React.useState<CredentialKind>("api_key")
  const [key, setKey] = React.useState("")
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      await onCreate(label.trim(), kind, key.trim())
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the credential")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>Add credential</DialogTitle>
          </DialogHeader>

          <AnimatedField
            id="credential-label"
            label="Label"
            value={label}
            onChange={setLabel}
            placeholder="anthropic-personal"
            required
            hint="Unique per user — used to pick this credential when adding a model or host."
          />

          <div className="w-full">
            <label htmlFor="credential-kind" className="mb-1.5 block text-sm font-medium text-foreground/80">
              Kind
            </label>
            <Select
              value={kind}
              onValueChange={(value) => setKind(value as CredentialKind)}
            >
              <SelectTrigger id="credential-kind"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="api_key">API key</SelectItem>
                <SelectItem value="ssh_key">SSH private key</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {kind === "ssh_key" ? (
            <div className="w-full">
              <label htmlFor="credential-key" className="mb-1.5 block text-sm font-medium text-foreground/80">
                SSH private key<span className="ml-0.5 text-accent-foreground/70">*</span>
              </label>
              <textarea
                id="credential-key"
                value={key}
                onChange={(event) => setKey(event.target.value)}
                placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                required
                rows={8}
                spellCheck={false}
                autoCapitalize="none"
                autoCorrect="off"
                className="min-h-44 w-full resize-y rounded-lg border border-border bg-card px-3 py-2.5 font-mono text-xs text-foreground outline-none transition focus:border-primary focus:ring-4 focus:ring-primary/10"
                aria-describedby="credential-key-hint"
              />
              <p id="credential-key-hint" className="mt-1.5 text-xs text-muted-foreground">
                Encrypted at rest — you won&apos;t be able to view it again.
              </p>
            </div>
          ) : (
            <AnimatedField
              id="credential-key"
              label="API key"
              type="password"
              value={key}
              onChange={setKey}
              placeholder="sk-…"
              required
              hint="Encrypted at rest — you won't be able to view it again."
            />
          )}

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="submit" loading={submitting} loadingText="Adding">
              Add credential
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
