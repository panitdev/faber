import * as React from "react"
import { createFileRoute } from "@tanstack/react-router"
import { Pencil, Plus, PlugZap, Power, Server, Trash2 } from "lucide-react"

import { type Host } from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
import { addressLabel } from "@/lib/hosts/labels"
import { Button } from "@/components/ui/button"
import { HostFormDialog } from "@/components/hosts/host-dialogs"
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

/**
 * The machines, and nothing else.
 *
 * Per `internal-docs/host.md` the host is the registration primitive: it is
 * what carries authentication, network path, and trust. What you can actually
 * run *in* — a direct host itself, or a container registered on a docker host —
 * is an environment, and lives on `/environments`. Keeping the two apart is
 * what stops this page from becoming a container manager by accident.
 */
export const Route = createFileRoute("/hosts")({ component: HostsPage })

function HostsPage() {
  const { hosts, loaded, error, addHost, editHost, removeHost } = useHosts()

  const [hostDialogOpen, setHostDialogOpen] = React.useState(false)
  const [editingHost, setEditingHost] = React.useState<Host | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Host | null>(null)
  // An agent host whose install command was never used — or was lost — has no
  // way back to one, since the token is shown once. This reopens the flow at
  // the install step with a freshly issued command.
  const [installTarget, setInstallTarget] = React.useState<Host | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  // Bumped on every open so the dialog remounts with a fresh draft — the same
  // trick the models page uses.
  const [formKey, setFormKey] = React.useState(0)
  const bump = () => setFormKey((k) => k + 1)

  const openCreateHost = () => {
    setEditingHost(null)
    setInstallTarget(null)
    bump()
    setHostDialogOpen(true)
  }

  const openEditHost = (host: Host) => {
    setEditingHost(host)
    setInstallTarget(null)
    bump()
    setHostDialogOpen(true)
  }

  const openInstall = (host: Host) => {
    setEditingHost(null)
    setInstallTarget(host)
    bump()
    setHostDialogOpen(true)
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await removeHost(deleteTarget.id)
      setDeleteTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeleting(false)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 px-4 py-10">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Hosts</h1>
            <p className="text-sm text-muted-foreground">
              The machines faber can reach. Every execution mode bottoms out in
              one of these.
            </p>
          </div>
          <Button size="sm" onClick={openCreateHost}>
            <Plus className="h-4 w-4" />
            Add host
          </Button>
        </div>

        {error ? <p className="text-sm text-destructive">{error}</p> : null}

        {!loaded ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : hosts.length === 0 ? (
          <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
            <Server className="h-6 w-6 text-muted-foreground" />
            <div>
              <p className="text-sm font-medium">No hosts yet</p>
              <p className="text-sm text-muted-foreground">
                Register one to give the agent somewhere to run.
              </p>
            </div>
            <Button size="sm" onClick={openCreateHost}>
              <Plus className="h-4 w-4" />
              Add host
            </Button>
          </div>
        ) : (
          <ul className="flex flex-col gap-3">
            {hosts.map((host) => (
              <HostCard
                key={host.id}
                host={host}
                onEdit={() => openEditHost(host)}
                onInstall={() => openInstall(host)}
                onDelete={() => setDeleteTarget(host)}
                onToggleDisabled={() =>
                  editHost(host.id, { disabled: !host.disabled_at }).catch(() => {
                    // Left as-is on failure; the row still shows the server's
                    // last known answer.
                  })
                }
              />
            ))}
          </ul>
        )}
      </div>

      <HostFormDialog
        key={`host-${formKey}`}
        open={hostDialogOpen}
        onOpenChange={setHostDialogOpen}
        editing={editingHost}
        installFor={installTarget}
        onCreate={addHost}
        onUpdate={editHost}
      />

      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              This drops the registration, its container registrations, and its
              probe history. Nothing on the machine itself is touched. Disabling
              the host instead keeps all of it and just takes it out of use.
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

function Badge({ children }: { children: React.ReactNode }) {
  return (
    <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
      {children}
    </span>
  )
}

function HostCard({
  host,
  onEdit,
  onInstall,
  onDelete,
  onToggleDisabled,
}: {
  host: Host
  onEdit: () => void
  onInstall: () => void
  onDelete: () => void
  onToggleDisabled: () => void
}) {
  const disabled = !!host.disabled_at

  return (
    <li className="rounded-xl border border-border bg-card">
      <div className="flex items-start justify-between gap-4 px-4 py-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate text-sm font-medium">{host.name}</span>
            <Badge>{host.transport}</Badge>
            <Badge>{host.exec_mode}</Badge>
            {disabled ? <Badge>disabled</Badge> : null}
          </div>
          <p className="truncate text-xs text-muted-foreground">
            {`${addressLabel(host)}${
              host.exec_mode === "docker" ? ` · ${host.docker_endpoint ?? "local socket"}` : ""
            }`}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {host.transport === "agent" ? (
            <Button
              size="icon-sm"
              variant="ghost"
              aria-label={`Install command for ${host.name}`}
              title="Install command"
              onClick={onInstall}
            >
              <PlugZap className="h-4 w-4" />
            </Button>
          ) : null}
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={disabled ? `Enable ${host.name}` : `Disable ${host.name}`}
            title={disabled ? "Enable" : "Disable"}
            onClick={onToggleDisabled}
          >
            <Power className="h-4 w-4" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={`Edit ${host.name}`}
            onClick={onEdit}
          >
            <Pencil className="h-4 w-4" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={`Delete ${host.name}`}
            onClick={onDelete}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </li>
  )
}
