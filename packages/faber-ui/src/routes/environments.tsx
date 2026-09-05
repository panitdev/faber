import * as React from "react"
import { createFileRoute } from "@tanstack/react-router"
import {
  Box,
  ChevronDown,
  Container,
  Layers,
  Pencil,
  Plus,
  Server,
  Sparkles,
  Trash2,
  Unplug,
} from "lucide-react"

import { type Host, type HostContainer, type Image } from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
import { useImages } from "@/lib/hosts/use-images"
import { addressLabel, observation, toolList } from "@/lib/hosts/labels"
import { Button } from "@/components/ui/button"
import {
  ContainerFormDialog,
  ContainerSpawnDialog,
  ImageFormDialog,
} from "@/components/hosts/host-dialogs"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
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
 * The environments themselves: the places a run can actually land.
 *
 * `internal-docs/host.md` puts it plainly — the registration primitive is the
 * host, but the *environment* is what you bind a run to. A direct host is one
 * environment, itself; a docker host contributes one per registered container
 * and none of its own. So every host registered on `/hosts` surfaces here, and
 * this is the only page that manages containers.
 *
 * The probe is stated as an observation, never as status. There is no green dot
 * here and nothing on this page should grow one: the authoritative answer to
 * "is it reachable" is the next connection attempt, and a light would invite
 * reading a cached one instead.
 */
export const Route = createFileRoute("/environments")({ component: EnvironmentsPage })

function EnvironmentsPage() {
  const {
    hosts,
    loaded,
    error,
    addContainer,
    spawnContainer,
    editContainer,
    unregisterContainer,
  } = useHosts()
  const images = useImages()

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [spawnOpen, setSpawnOpen] = React.useState(false)
  const [dialogHost, setDialogHost] = React.useState<Host | null>(null)
  const [editing, setEditing] = React.useState<HostContainer | null>(null)
  const [formKey, setFormKey] = React.useState(0)

  const [unregisterTarget, setUnregisterTarget] = React.useState<
    { host: Host; container: HostContainer; destroy: boolean } | null
  >(null)
  const [unregistering, setUnregistering] = React.useState(false)

  const openAdd = (host: Host) => {
    setDialogHost(host)
    setEditing(null)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const openCreate = (host: Host) => {
    setDialogHost(host)
    setFormKey((k) => k + 1)
    setSpawnOpen(true)
  }

  const openEdit = (host: Host, container: HostContainer) => {
    setDialogHost(host)
    setEditing(container)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const handleUnregister = async () => {
    if (!unregisterTarget) return
    setUnregistering(true)
    try {
      await unregisterContainer(
        unregisterTarget.host.id,
        unregisterTarget.container.id,
        unregisterTarget.destroy,
      )
      setUnregisterTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setUnregistering(false)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-10 px-4 py-10">
        <section className="flex flex-col gap-6">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Environments</h1>
            <p className="text-sm text-muted-foreground">
              Where a run can land. Every host you register shows up here.
            </p>
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          {!loaded ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : hosts.length === 0 ? (
            <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
              <Server className="h-6 w-6 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Nowhere to run yet</p>
                <p className="text-sm text-muted-foreground">
                  Register a host first — environments hang off one.
                </p>
              </div>
            </div>
          ) : (
            <ul className="flex flex-col gap-3">
              {hosts.map((host) => (
                <HostEnvironments
                  key={host.id}
                  host={host}
                  onAdd={() => openAdd(host)}
                  onCreate={() => openCreate(host)}
                  onEdit={(container) => openEdit(host, container)}
                  onUnregister={(container, destroy) =>
                    setUnregisterTarget({ host, container, destroy })
                  }
                />
              ))}
            </ul>
          )}
        </section>

        <ImagesSection images={images} />
      </div>

      <ContainerSpawnDialog
        key={`spawn-${formKey}`}
        open={spawnOpen}
        onOpenChange={setSpawnOpen}
        host={dialogHost}
        images={images.images}
        onSpawn={spawnContainer}
      />

      <ContainerFormDialog
        key={`container-${formKey}`}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        host={dialogHost}
        editing={editing}
        onCreate={addContainer}
        onUpdate={editContainer}
      />

      <AlertDialog
        open={!!unregisterTarget}
        onOpenChange={(open) => !open && setUnregisterTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {unregisterTarget?.destroy ? "Delete" : "Unregister"}{" "}
              {unregisterTarget?.container.name ?? unregisterTarget?.container.container_ref}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              {unregisterTarget?.destroy
                ? "The container is removed from the machine and everything inside it goes with it. Your work and scratch directories are mounts and survive — anything written outside them does not."
                : "Faber forgets the container and stops offering it as an environment. The container itself keeps running — nothing on the host is touched."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={unregistering}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void handleUnregister()
              }}
              disabled={unregistering}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {unregistering
                ? "Working…"
                : unregisterTarget?.destroy
                  ? "Delete"
                  : "Unregister"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

/**
 * One host's contribution to the list.
 *
 * A direct host *is* the environment, so it renders as a single row with
 * nothing to manage — its registration lives on `/hosts`. A docker host is a
 * container of environments rather than one itself, so it renders as a header
 * over its registrations.
 */
function HostEnvironments({
  host,
  onAdd,
  onCreate,
  onEdit,
  onUnregister,
}: {
  host: Host
  onAdd: () => void
  onCreate: () => void
  onEdit: (container: HostContainer) => void
  onUnregister: (container: HostContainer, destroy: boolean) => void
}) {
  const disabled = !!host.disabled_at
  const tools = toolList(host.last_probe)

  return (
    <li className="rounded-xl border border-border bg-card">
      <div className="flex items-start justify-between gap-4 px-4 py-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate text-sm font-medium">{host.name}</span>
            <Badge>{host.exec_mode}</Badge>
            {disabled ? <Badge>disabled</Badge> : null}
          </div>
          {/* Past tense with its age, because that is all a probe is.
              "Never probed" is its own answer — not a default to "down". */}
          <p className="truncate text-xs text-muted-foreground">
            {addressLabel(host)} · {observation(host.last_probe)}
          </p>
        </div>
        {/* Two ways to get a container, and the difference is who created it:
            Create starts one from an image, Add adopts one already running. */}
        {host.exec_mode === "docker" ? (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button size="sm" variant="ghost" className="shrink-0">
                <Plus className="h-4 w-4" />
                Add
                <ChevronDown className="h-3.5 w-3.5 opacity-60" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-64">
              <DropdownMenuItem className="items-start gap-2.5 py-2" onSelect={onCreate}>
                <Sparkles className="mt-0.5 h-4 w-4" />
                <div>
                  <p>Create</p>
                  <p className="text-xs text-muted-foreground">
                    Start a new one from an image.
                  </p>
                </div>
              </DropdownMenuItem>
              <DropdownMenuItem className="items-start gap-2.5 py-2" onSelect={onAdd}>
                <Container className="mt-0.5 h-4 w-4" />
                <div>
                  <p>Add</p>
                  <p className="text-xs text-muted-foreground">
                    Point faber at one you already run.
                  </p>
                </div>
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
      </div>

      {tools.length > 0 ? (
        <div className="flex flex-wrap items-center gap-1.5 px-4 pb-3">
          {tools.map(([name, version]) => (
            <span
              key={name}
              className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
            >
              {name} {version}
            </span>
          ))}
        </div>
      ) : null}

      {host.exec_mode === "docker" ? (
        <div className="border-t border-border px-4 py-3">
          {host.containers.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              No containers registered on this host.
            </p>
          ) : (
            <ul className="flex flex-col gap-1">
              {host.containers.map((container) => (
                <li
                  key={container.id}
                  className="flex items-center justify-between gap-3 rounded-lg px-2 py-1.5 hover:bg-muted/50"
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <Container className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                    <span className="truncate text-[13px]">
                      {container.name ?? container.container_ref}
                    </span>
                    <span className="truncate font-mono text-[11px] text-muted-foreground">
                      {container.root_path}
                    </span>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Edit ${container.container_ref}`}
                      onClick={() => onEdit(container)}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                    </Button>
                    {/* Offered exactly when the server will accept it: faber
                        destroys only what it created. */}
                    {container.managed ? (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        aria-label={`Delete ${container.container_ref}`}
                        title="Delete — the container is removed from the machine"
                        onClick={() => onUnregister(container, true)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    ) : (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        aria-label={`Unregister ${container.container_ref}`}
                        title="Unregister — the container itself keeps running"
                        onClick={() => onUnregister(container, false)}
                      >
                        <Unplug className="h-3.5 w-3.5" />
                      </Button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </li>
  )
}

function Badge({ children }: { children: React.ReactNode }) {
  return (
    <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
      {children}
    </span>
  )
}

/**
 * Spawn templates: a saved reference plus its defaults, and the thing Create
 * starts a container from.
 *
 * It sits with the environments rather than the hosts because an image
 * describes a container, and a container is an environment.
 */
function ImagesSection({ images: source }: { images: ReturnType<typeof useImages> }) {
  const { images, loaded, error, addImage, editImage, removeImage } = source

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<Image | null>(null)
  const [formKey, setFormKey] = React.useState(0)
  const [deleteTarget, setDeleteTarget] = React.useState<Image | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await removeImage(deleteTarget.id)
      setDeleteTarget(null)
    } catch {
      // The hook holds the message; the dialog stays open for a retry.
    } finally {
      setDeleting(false)
    }
  }

  const openCreate = () => {
    setEditing(null)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const openEdit = (image: Image) => {
    setEditing(image)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  return (
    <section className="flex flex-col gap-6 border-t border-border pt-10">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold tracking-tight">Images</h2>
          <p className="text-sm text-muted-foreground">
            Saved templates for containers you start yourself.
          </p>
        </div>
        <Button size="sm" onClick={openCreate}>
          <Plus className="h-4 w-4" />
          Add image
        </Button>
      </div>

      {error ? <p className="text-sm text-destructive">{error}</p> : null}

      {!loaded ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : images.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-12 text-center">
          <Layers className="h-6 w-6 text-muted-foreground" />
          <p className="text-sm text-muted-foreground">No images saved.</p>
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {images.map((image) => (
            <li
              key={image.id}
              className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
            >
              <div className="flex min-w-0 items-center gap-2.5">
                <Box className="h-4 w-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <p className="truncate text-sm font-medium">{image.name}</p>
                  </div>
                  <p className="truncate text-xs text-muted-foreground">
                    {image.reference} · {image.default_root_path}
                  </p>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <Button
                  size="icon-sm"
                  variant="ghost"
                  aria-label={`Edit ${image.name}`}
                  onClick={() => openEdit(image)}
                >
                  <Pencil className="h-4 w-4" />
                </Button>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  aria-label={`Delete ${image.name}`}
                  onClick={() => setDeleteTarget(image)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <ImageFormDialog
        key={`image-${formKey}`}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        editing={editing}
        onCreate={addImage}
        onUpdate={editImage}
      />

      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Containers you already started from this template are unaffected —
              nothing links them back to it.
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
