import * as React from "react"
import { createFileRoute } from "@tanstack/react-router"
import {
  Container,
  Laptop,
  Link2,
  MoreHorizontal,
  Pencil,
  Plus,
  PlugZap,
  Power,
  Server,
  Sparkles,
  SquareTerminal,
  Trash2,
  Unlink2,
  Unplug,
  type LucideIcon,
} from "lucide-react"

import { type ExecMode, type Host, type HostContainer, type Transport } from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
import { addressLabel, observation, toolList } from "@/lib/hosts/labels"
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
  ContainerFormDialog,
  ContainerSpawnDialog,
  HostFormDialog,
} from "@/components/hosts/host-dialogs"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
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
 * The environments, and the hosts they hang off.
 *
 * `internal-docs/host.md` puts it plainly — the registration primitive is the
 * host, but the *environment* is what you bind a run to. A direct host is one
 * environment, itself; a docker host contributes one per registered container
 * and none of its own. Hosts are registered and managed here too, as the
 * parent rows their environments sit under, so the machine and what runs on it
 * are never a page apart.
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
    addHost,
    editHost,
    removeHost,
    addContainer,
    spawnContainer,
    editContainer,
    unregisterContainer,
  } = useHosts()

  const [hostDialogOpen, setHostDialogOpen] = React.useState(false)
  const [editingHost, setEditingHost] = React.useState<Host | null>(null)
  // An agent host whose install command was never used — or was lost — has no
  // way back to one, since the token is shown once. This reopens the flow at
  // the install step with a freshly issued command.
  const [installTarget, setInstallTarget] = React.useState<Host | null>(null)
  const [deleteHostTarget, setDeleteHostTarget] = React.useState<Host | null>(null)
  const [deletingHost, setDeletingHost] = React.useState(false)

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [spawnOpen, setSpawnOpen] = React.useState(false)
  const [dialogHost, setDialogHost] = React.useState<Host | null>(null)
  const [editing, setEditing] = React.useState<HostContainer | null>(null)
  const [formKey, setFormKey] = React.useState(0)

  const [unregisterTarget, setUnregisterTarget] = React.useState<
    { host: Host; container: HostContainer; destroy: boolean } | null
  >(null)
  const [unregistering, setUnregistering] = React.useState(false)

  const openHostDialog = (editing: Host | null, installFor: Host | null) => {
    setEditingHost(editing)
    setInstallTarget(installFor)
    setFormKey((k) => k + 1)
    setHostDialogOpen(true)
  }

  const handleDeleteHost = async () => {
    if (!deleteHostTarget) return
    setDeletingHost(true)
    try {
      await removeHost(deleteHostTarget.id)
      setDeleteHostTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeletingHost(false)
    }
  }

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
      <section className="mx-auto flex w-full max-w-2xl flex-col gap-6 px-4 py-6 md:py-10">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Environments</h1>
            <p className="text-sm text-muted-foreground">
              Where a run can land, grouped by the host that provides it.
            </p>
          </div>
          <Button
            size="sm"
            className="w-full sm:w-auto"
            onClick={() => openHostDialog(null, null)}
          >
            <Plus className="h-4 w-4" />
            Add host
          </Button>
        </div>

        {error ? <p className="text-sm text-destructive">{error}</p> : null}

        {/* Fixed layout: long names, addresses, and paths truncate in their
            column instead of widening the table past the page. */}
        <Table className="table-fixed">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead className="hidden w-36 sm:table-cell">Last probe</TableHead>
              <TableHead className="w-24">
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {!loaded ? (
              <TableLoading columns={3} />
            ) : hosts.length === 0 ? (
              <TableEmpty
                colSpan={3}
                icon={<Server />}
                title="Nowhere to run yet"
                description="Register a host first — environments hang off one."
                action={
                  <Button size="sm" onClick={() => openHostDialog(null, null)}>
                    <Plus className="h-4 w-4" />
                    Add host
                  </Button>
                }
              />
            ) : (
              hosts.map((host) => (
                <HostRows
                  key={host.id}
                  host={host}
                  onEditHost={() => openHostDialog(host, null)}
                  onInstall={() => openHostDialog(null, host)}
                  onDeleteHost={() => setDeleteHostTarget(host)}
                  onToggleDisabled={() =>
                    editHost(host.id, { disabled: !host.disabled_at }).catch(() => {
                      // Left as-is on failure; the row still shows the
                      // server's last known answer.
                    })
                  }
                  onAdd={() => openAdd(host)}
                  onCreate={() => openCreate(host)}
                  onEdit={(container) => openEdit(host, container)}
                  onUnregister={(container, destroy) =>
                    setUnregisterTarget({ host, container, destroy })
                  }
                />
              ))
            )}
          </TableBody>
        </Table>
      </section>

      <HostFormDialog
        key={`host-${formKey}`}
        open={hostDialogOpen}
        onOpenChange={setHostDialogOpen}
        editing={editingHost}
        installFor={installTarget}
        onCreate={addHost}
        onUpdate={editHost}
      />

      <AlertDialog
        open={!!deleteHostTarget}
        onOpenChange={(open) => !open && setDeleteHostTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteHostTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              This drops the registration, its container registrations, and its
              probe history. Nothing on the machine itself is touched. Disabling
              the host instead keeps all of it and just takes it out of use.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deletingHost}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void handleDeleteHost()
              }}
              disabled={deletingHost}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {deletingHost ? "Deleting…" : "Delete"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <ContainerSpawnDialog
        key={`spawn-${formKey}`}
        open={spawnOpen}
        onOpenChange={setSpawnOpen}
        host={dialogHost}
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
                : "Panit forgets the container and stops offering it as an environment. The container itself keeps running — nothing on the host is touched."}
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
 * One host and the environments it contributes, as rows of the one table.
 *
 * A direct host *is* its environment, so it is a single row. A docker host is
 * not one itself: its row is a parent, and each
 * registered container is an environment indented beneath it.
 */
function HostRows({
  host,
  onEditHost,
  onInstall,
  onDeleteHost,
  onToggleDisabled,
  onAdd,
  onCreate,
  onEdit,
  onUnregister,
}: {
  host: Host
  onEditHost: () => void
  onInstall: () => void
  onDeleteHost: () => void
  onToggleDisabled: () => void
  onAdd: () => void
  onCreate: () => void
  onEdit: (container: HostContainer) => void
  onUnregister: (container: HostContainer, destroy: boolean) => void
}) {
  const disabled = !!host.disabled_at
  const docker = host.exec_mode === "docker"
  // On a direct host the probe ran where the agent runs, so its tools are the
  // environment's. On a docker host they describe the daemon machine, not its
  // containers, so they are left out rather than shown above them.
  const tools = docker
    ? ""
    : toolList(host.last_probe)
        .map(([name, version]) => `${name} ${version}`)
        .join(", ")
  // The socket path is long and rarely what you came for; it goes in the
  // tooltip so the observation is not the part that gets cut off.
  const address = addressLabel(host)
  const probe = observation(host.last_probe)
  const details = docker ? `Docker endpoint: ${host.docker_endpoint ?? "local socket"}` : tools
  // Disabled is operator intent, and everything under the host goes out of
  // use with it — so the rows dim, not just a badge.
  const tone = disabled ? "text-muted-foreground" : undefined

  return (
    <>
      <TableRow className={tone}>
        <TableCell className="py-3.5">
          {/* Badges wrap under the name when the row is narrow, so the name is
              never the thing that gets cut to make room for them. */}
          <div className="flex items-start gap-2" title={details || undefined}>
            <Server className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
            <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
              <span className="max-w-full min-w-0 truncate font-medium">{host.name}</span>
              <span className="inline-flex flex-wrap items-center gap-1.5">
                <Badge icon={TRANSPORT_ICON[host.transport]}>{host.transport}</Badge>
                <Badge icon={EXEC_MODE_ICON[host.exec_mode]}>{host.exec_mode}</Badge>
                {disabled ? <Badge icon={Power}>disabled</Badge> : null}
                {!docker && host.bind_by_default ? <Badge icon={Link2}>auto-bind</Badge> : null}
                {/* A direct host is bound at its root path; without one there
                    is nothing here a session can bind. */}
                {!docker && !host.root_path ? <Badge icon={Unlink2}>not bindable</Badge> : null}
              </span>
            </div>
          </div>
          {address ? (
            <p className="mt-0.5 truncate pl-6 text-xs text-muted-foreground">{address}</p>
          ) : null}
          {probe ? (
            <p className="mt-0.5 truncate pl-6 text-xs text-muted-foreground sm:hidden">{probe}</p>
          ) : null}
        </TableCell>
        {/* Past tense with its age, because that is all a probe is. A host
            never probed shows an empty cell — not a default to "down". */}
        <TableCell className="hidden py-3.5 align-top text-xs whitespace-normal text-muted-foreground sm:table-cell">
          <p className="line-clamp-2 pt-0.5">{probe ?? "—"}</p>
        </TableCell>
        <TableCell align="end" className="w-px">
          <TableRowActions>
            {/* Two ways to get a container, and the difference is who created
                it: Create starts one from an image, Add adopts one already
                running. */}
            {docker ? (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button size="icon-sm" variant="ghost" aria-label={`Add a container to ${host.name}`}>
                    <Plus className="h-4 w-4" />
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
                        Point Panit at one you already run.
                      </p>
                    </div>
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            ) : null}
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button size="icon-sm" variant="ghost" aria-label={`More actions for ${host.name}`}>
                  <MoreHorizontal className="h-4 w-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {host.transport === "agent" ? (
                  <DropdownMenuItem onSelect={onInstall}>
                    <PlugZap className="h-4 w-4" />
                    Install command
                  </DropdownMenuItem>
                ) : null}
                <DropdownMenuItem onSelect={onEditHost}>
                  <Pencil className="h-4 w-4" />
                  Edit host
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={onToggleDisabled}>
                  <Power className="h-4 w-4" />
                  {disabled ? "Enable" : "Disable"}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={onDeleteHost}>
                  <Trash2 className="h-4 w-4" />
                  Delete host
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </TableRowActions>
        </TableCell>
      </TableRow>

      {docker && host.containers.length === 0 ? (
        <TableRow className={tone}>
          <TableCell colSpan={3} className="py-3">
            <p className="pl-6 text-xs text-muted-foreground">
              No containers yet — create one, or add one you already run.
            </p>
          </TableCell>
        </TableRow>
      ) : null}

      {docker
        ? host.containers.map((container) => (
            <TableRow key={container.id} className={tone}>
              <TableCell className="py-3">
                <div className="flex items-start gap-2 pl-6">
                  <Container className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                  <div className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
                    <span className="max-w-full min-w-0 truncate">
                      {container.name ?? container.container_ref}
                    </span>
                    {container.image ? (
                      <span className="max-w-full min-w-0 truncate font-mono text-[11px] text-muted-foreground">
                        {container.image}
                      </span>
                    ) : null}
                    {container.bind_by_default ? <Badge icon={Link2}>auto-bind</Badge> : null}
                  </div>
                </div>
              </TableCell>
              <TableCell className="hidden py-3 sm:table-cell" />
              <TableCell align="end" className="w-px">
                <TableRowActions>
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
                </TableRowActions>
              </TableCell>
            </TableRow>
          ))
        : null}
    </>
  )
}

/** The same icons the host dialog uses when you pick a transport. */
const TRANSPORT_ICON: Record<Transport, LucideIcon> = {
  local: Laptop,
  ssh: Server,
  agent: PlugZap,
}

const EXEC_MODE_ICON: Record<ExecMode, LucideIcon> = {
  direct: SquareTerminal,
  docker: Container,
}

function Badge({ icon: Icon, children }: { icon: LucideIcon; children: React.ReactNode }) {
  return (
    <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
      <Icon className="size-3" />
      {children}
    </span>
  )
}
