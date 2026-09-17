"use client"

import * as React from "react"
import { Laptop, PlugZap, Server } from "lucide-react"

import {
  FaberError,
  type CreateContainerRequest,
  type CreateHostRequest,
  type CreateImageRequest,
  type Credential,
  type ExecMode,
  type Host,
  type HostContainer,
  type Image,
  type SpawnContainerRequest,
  type Transport,
  type UpdateContainerRequest,
  type UpdateHostRequest,
  type UpdateImageRequest,
  type Uuid,
  faber,
} from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import { AgentConnected, AgentInstall } from "@/components/hosts/agent-install"
import { ActionRow, ActionRowGroup } from "@/components/ui/action-row"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { AnimatedField } from "@/components/ui/animated-field"
import {
  FlowDialog,
  FlowDialogBack,
  FlowDialogHeading,
} from "@/components/ui/flow-dialog"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

function Field({
  id,
  label,
  children,
  hint,
}: {
  id: string
  label: string
  children: React.ReactNode
  hint?: string
}) {
  return (
    <div className="w-full">
      <label htmlFor={id} className="mb-1.5 block text-sm font-medium text-foreground/80">
        {label}
      </label>
      {children}
      {hint ? <p className="mt-1.5 text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

type HostFormState = {
  name: string
  transport: Transport
  exec_mode: ExecMode
  ssh_address: string
  ssh_key_ref: string
  docker_endpoint: string
  root_path: string
  bind_by_default: boolean
}

const EMPTY_HOST_FORM: HostFormState = {
  name: "",
  transport: "local",
  exec_mode: "direct",
  ssh_address: "",
  ssh_key_ref: "",
  docker_endpoint: "",
  root_path: "",
  bind_by_default: false,
}

function formFromHost(host: Host): HostFormState {
  return {
    name: host.name,
    transport: host.transport,
    exec_mode: host.exec_mode,
    ssh_address: host.ssh_address ?? "",
    ssh_key_ref: host.ssh_key_ref ?? "",
    docker_endpoint: host.docker_endpoint ?? "",
    root_path: host.root_path ?? "",
    bind_by_default: host.bind_by_default,
  }
}

/**
 * `ssh_address` is sent only on an SSH host: the server rejects it on a local
 * one, and clearing it here is what lets a host switch transports in one save.
 * An agent host is addressless for a different reason than a local one — faber
 * never dials it at all — but the request looks the same either way.
 */
function requestFromHostForm(form: HostFormState): CreateHostRequest {
  const ssh = form.transport === "ssh"
  return {
    name: form.name.trim(),
    transport: form.transport,
    exec_mode: form.exec_mode,
    ssh_address: ssh ? form.ssh_address.trim() : null,
    ssh_key_ref: ssh && form.ssh_key_ref.trim() ? form.ssh_key_ref.trim() : null,
    docker_endpoint:
      form.exec_mode === "docker" && form.docker_endpoint.trim()
        ? form.docker_endpoint.trim()
        : null,
    root_path:
      form.exec_mode === "direct" && form.root_path.trim()
        ? form.root_path.trim()
        : null,
    bind_by_default: form.bind_by_default,
  }
}

export function HostFormDialog({
  open,
  onOpenChange,
  editing,
  installFor,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  editing: Host | null
  /**
   * An agent host to issue a fresh install command for, skipping the form
   * entirely. This is how an agent host whose daemon was never installed —
   * or whose command expired unused — gets a second one, since the token in
   * the first is shown once and never stored.
   */
  installFor?: Host | null
  onCreate: (body: CreateHostRequest) => Promise<Host>
  onUpdate: (id: Uuid, patch: UpdateHostRequest) => Promise<Host>
}) {
  // The parent remounts this via `key` on each open, so the lazy initializer
  // alone seeds a fresh draft.
  const [form, setForm] = React.useState<HostFormState>(() =>
    editing ? formFromHost(editing) : EMPTY_HOST_FORM,
  )
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)
  const [credentials, setCredentials] = React.useState<Credential[]>([])
  const [view, setView] = React.useState(
    installFor ? "install" : editing ? "form" : "transport",
  )
  // The host the install command belongs to: the one just created, or the one
  // the caller asked to re-issue for.
  const [agentHost, setAgentHost] = React.useState<Host | null>(installFor ?? null)
  const { config } = useAppShell()
  const allowLocalHosts = config?.allow_local_hosts ?? true
  const transport =
    !editing && !allowLocalHosts && form.transport === "local" ? "ssh" : form.transport
  React.useEffect(() => {
    if (transport !== "ssh") return
    void faber.listCredentials("ssh_key").then(setCredentials).catch(() => setCredentials([]))
  }, [transport])

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      const body = requestFromHostForm({ ...form, transport })
      if (editing) {
        await onUpdate(editing.id, body)
      } else {
        const created = await onCreate(body)
        // An agent host is not usable the moment it is saved: the machine has
        // to dial in, and it cannot until somebody installs the daemon. So the
        // flow continues rather than closing on the row being written.
        if (created.transport === "agent") {
          setAgentHost(created)
          setView("install")
          return
        }
      }
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the host")
    } finally {
      setSubmitting(false)
    }
  }

  const chooseTransport = (nextTransport: Transport) => {
    setForm((current) => ({ ...current, transport: nextTransport }))
    setView("form")
  }

  const flowView = editing ? "form" : view
  // Stable, so the install view's poll keeps its interval instead of
  // restarting it on every parent render.
  const handleConnected = React.useCallback(() => setView("connected"), [])

  return (
    <FlowDialog
      open={open}
      onOpenChange={onOpenChange}
      view={flowView}
      title={editing ? `Edit ${editing.name}` : "Add host"}
    >
      {flowView === "install" && agentHost ? (
        <AgentInstall
          hostName={agentHost.name}
          description="Run this on the machine. It installs a user service — no root, and nothing to open in a firewall."
          open={open}
          reentry={!!installFor}
          issue={() => faber.enrollAgentHost(agentHost.id)}
          status={() => faber.agentStatus(agentHost.id)}
          onConnected={handleConnected}
        />
      ) : flowView === "connected" && agentHost ? (
        <AgentConnected hostName={agentHost.name} onDone={() => onOpenChange(false)} />
      ) : !editing && view === "transport" ? (
        <div>
          <FlowDialogHeading
            title="How should faber reach this host?"
            description="Choose the transport first. You can fill in the connection details next."
          />
          <ActionRowGroup>
            <ActionRow
              icon={<Laptop />}
              label="Local"
              description={`This machine${!allowLocalHosts ? " (disabled)" : ""}`}
              chevron
              disabled={!allowLocalHosts}
              onSelect={() => chooseTransport("local")}
            />
            <ActionRow
              icon={<Server />}
              label="SSH"
              description="Connect over SSH to a remote machine"
              chevron
              onSelect={() => chooseTransport("ssh")}
            />
            <ActionRow
              icon={<PlugZap />}
              label="Agent"
              description="A daemon you install dials out to faber"
              chevron
              onSelect={() => chooseTransport("agent")}
            />
          </ActionRowGroup>
        </div>
      ) : (
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <FlowDialogHeading
            title={editing ? `Edit ${editing.name}` : "Host details"}
            description={
              editing
                ? undefined
                : transport === "ssh"
                  ? "Connect to this host over SSH."
                  : transport === "agent"
                    ? "Name it first. The install command comes next, and the machine dials faber itself."
                    : "Run faber directly on this machine."
            }
          />

          <AnimatedField
            id="host-name"
            label="Name"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="workbench"
            required
          />

          {editing ? (
            <Field
              id="host-transport"
              label="Transport"
              hint="How faber reaches the machine."
            >
              <Select
                value={transport}
                onValueChange={(value) => setForm((f) => ({ ...f, transport: value as Transport }))}
              >
                <SelectTrigger id="host-transport">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="local">local — this machine</SelectItem>
                  <SelectItem value="ssh">ssh — a remote machine</SelectItem>
                  <SelectItem value="agent">agent — a daemon that dials faber</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          ) : null}

           {transport === "ssh" ? (
            <>
              <AnimatedField
                id="host-ssh-address"
                label="SSH address"
                value={form.ssh_address}
                onChange={(v) => setForm((f) => ({ ...f, ssh_address: v }))}
                placeholder="user@host:22"
                required
              />
              <Field
                id="host-ssh-key-ref"
                label="SSH key"
                hint="Select an SSH private-key credential."
              >
                <Select
                  value={form.ssh_key_ref}
                  onValueChange={(value) => setForm((f) => ({ ...f, ssh_key_ref: value }))}
                >
                   <SelectTrigger id="host-ssh-key-ref" aria-required="true">
                     <SelectValue placeholder="Select a credential" />
                   </SelectTrigger>
                   <SelectContent>
                     {credentials.map((credential) => (
                       <SelectItem key={credential.id} value={credential.id}>
                         {credential.label} ····{credential.last_four}
                       </SelectItem>
                     ))}
                  </SelectContent>
                </Select>
                {credentials.every((credential) => credential.kind !== "ssh_key") ? (
                  <p className="mt-1.5 text-xs text-muted-foreground">
                    Add an SSH private-key credential first in Credentials.
                  </p>
                ) : null}
              </Field>
            </>
          ) : null}

          <Field
            id="host-exec-mode"
            label="Execution mode"
            hint="What faber execs into once it has reached the machine. This is a choice, not a consequence of the endpoint below."
          >
            <Select
              value={form.exec_mode}
              onValueChange={(value) =>
                setForm((f) => ({ ...f, exec_mode: value as ExecMode }))
              }
            >
              <SelectTrigger id="host-exec-mode">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="direct">direct — the machine&apos;s own filesystem</SelectItem>
                <SelectItem value="docker">docker — containers on the machine</SelectItem>
              </SelectContent>
            </Select>
          </Field>

          {form.exec_mode === "docker" ? (
            <AnimatedField
              id="host-docker-endpoint"
              label="Docker endpoint"
              value={form.docker_endpoint}
              onChange={(v) => setForm((f) => ({ ...f, docker_endpoint: v }))}
              placeholder="unix:///var/run/docker.sock"
              required
              hint="Required. Use unix:// for a local socket or tcp:// for a reachable Docker daemon."
            />
          ) : null}

          {form.exec_mode === "direct" ? (
            <AnimatedField
              id="host-root-path"
              label="Root path"
              value={form.root_path}
              onChange={(v) => setForm((f) => ({ ...f, root_path: v }))}
              placeholder="/workspace"
              required
              hint="Absolute path the agent can see on this machine."
            />
          ) : null}

          {form.exec_mode === "direct" ? (
            <p className="rounded-lg border border-border bg-muted/40 px-3 py-2.5 text-xs text-muted-foreground">
              Direct mode gives the agent the whole filesystem this account can
              reach. Anything you take away is taken away by convention — the
              harness subtracts it, nothing below it does.
            </p>
          ) : null}

          <label className="flex items-center gap-2.5">
            <Checkbox
              checked={form.bind_by_default}
              onCheckedChange={(checked) =>
                setForm((f) => ({ ...f, bind_by_default: checked === true }))
              }
            />
            <span className="text-sm">Bind to new sessions by default</span>
          </label>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <div
            className={
              !editing
                ? "flex items-center justify-between [&>div]:mt-0 [&>div]:items-start"
                : undefined
            }
          >
            {!editing ? <FlowDialogBack onClick={() => setView("transport")} /> : null}
            <DialogFooter>
              <Button type="submit" loading={submitting} loadingText={editing ? "Saving" : "Adding"}>
                {editing ? "Save" : transport === "agent" ? "Continue" : "Add host"}
              </Button>
            </DialogFooter>
          </div>
        </form>
      )}
    </FlowDialog>
  )
}

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

type ContainerFormState = {
  container_ref: string
  name: string
  root_path: string
  bind_by_default: boolean
}

const EMPTY_CONTAINER_FORM: ContainerFormState = {
  container_ref: "",
  name: "",
  root_path: "",
  bind_by_default: false,
}

export function ContainerFormDialog({
  open,
  onOpenChange,
  host,
  editing,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  host: Host | null
  editing: HostContainer | null
  onCreate: (hostId: Uuid, body: CreateContainerRequest) => Promise<HostContainer>
  onUpdate: (
    hostId: Uuid,
    id: Uuid,
    patch: UpdateContainerRequest,
  ) => Promise<HostContainer>
}) {
  const [form, setForm] = React.useState<ContainerFormState>(() =>
    editing
      ? {
          container_ref: editing.container_ref,
          name: editing.name ?? "",
          root_path: editing.root_path,
          bind_by_default: editing.bind_by_default,
        }
      : EMPTY_CONTAINER_FORM,
  )
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!host) return
    setSubmitting(true)
    setError(null)
    try {
      const body = {
        container_ref: form.container_ref.trim(),
        name: form.name.trim() ? form.name.trim() : null,
        root_path: form.root_path.trim(),
        bind_by_default: form.bind_by_default,
      }
      if (editing) await onUpdate(host.id, editing.id, body)
      else await onCreate(host.id, body)
      onOpenChange(false)
    } catch (err) {
      setError(
        err instanceof FaberError ? err.message : "failed to save the registration",
      )
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>
              {editing ? "Edit registration" : `Add a container on ${host?.name ?? ""}`}
            </DialogTitle>
          </DialogHeader>

          <p className="text-sm text-muted-foreground">
            This records a container faber should know about. It does not create
            or start one — you own the container, faber only reaches it.
          </p>

          <AnimatedField
            id="container-ref"
            label="Container"
            value={form.container_ref}
            onChange={(v) => setForm((f) => ({ ...f, container_ref: v }))}
            placeholder="my-dev-box"
            required
            hint="Name or id. It is resolved when faber connects, not now."
          />

          <AnimatedField
            id="container-name"
            label="Label"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="Optional"
          />

          <AnimatedField
            id="container-root-path"
            label="Root path"
            value={form.root_path}
            onChange={(v) => setForm((f) => ({ ...f, root_path: v }))}
            placeholder="/workspace"
            required
            hint="Absolute, and normalized to what the agent should see — bind-mounted and native paths both arrive here."
          />

          <label className="flex items-center gap-2.5">
            <Checkbox
              checked={form.bind_by_default}
              onCheckedChange={(checked) =>
                setForm((f) => ({ ...f, bind_by_default: checked === true }))
              }
            />
            <span className="text-sm">Bind to new sessions by default</span>
          </label>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="submit" loading={submitting} loadingText="Saving">
              {editing ? "Save" : "Add"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// Container — spawned
// ---------------------------------------------------------------------------

type SpawnFormState = {
  image_id: string
  name: string
  root_path: string
}

/**
 * The "Create" half of the Add menu: faber starts the container instead of
 * adopting one the user already runs.
 *
 * Root path is seeded from the chosen image and stays editable, so the common
 * case is one click and the uncommon one is still reachable. The image list is
 * passed in rather than fetched here — the Environments page already holds it
 * for its own section.
 */
export function ContainerSpawnDialog({
  open,
  onOpenChange,
  host,
  images,
  onSpawn,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  host: Host | null
  images: Image[]
  onSpawn: (hostId: Uuid, body: SpawnContainerRequest) => Promise<HostContainer>
}) {
  const [form, setForm] = React.useState<SpawnFormState>({
    image_id: "",
    name: "",
    root_path: "",
  })
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  // Only follows the image while the user has not typed a root of their own:
  // an edited path survives switching images, an untouched one keeps tracking.
  const [rootTouched, setRootTouched] = React.useState(false)

  const selectImage = (id: string) => {
    const image = images.find((row) => row.id === id)
    setForm((f) => ({
      ...f,
      image_id: id,
      root_path: rootTouched ? f.root_path : (image?.default_root_path ?? ""),
    }))
  }

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!host || !form.image_id) return
    setSubmitting(true)
    setError(null)
    try {
      await onSpawn(host.id, {
        image_id: form.image_id,
        name: form.name.trim() ? form.name.trim() : null,
        root_path: form.root_path.trim(),
      })
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to create the container")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>Create a container on {host?.name ?? ""}</DialogTitle>
          </DialogHeader>

          <p className="text-sm text-muted-foreground">
            Faber starts this one from an image and registers what it started.
          </p>

          {images.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No images saved yet — add one below first, and it becomes available
              here.
            </p>
          ) : (
            <Field
              id="spawn-image"
              label="Image"
              hint="Saved templates. The reference is pulled on the host, not here."
            >
              <Select
                value={form.image_id}
                onValueChange={selectImage}
              >
                <SelectTrigger id="spawn-image" aria-required="true">
                  <SelectValue placeholder="Choose an image" />
                </SelectTrigger>
                <SelectContent>
                  {images.map((image) => (
                    <SelectItem key={image.id} value={image.id}>
                    {image.name} — {image.reference}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          )}

          <AnimatedField
            id="spawn-name"
            label="Name"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="Optional"
            hint="Used as the container's name on the daemon, and as its label here."
          />

          <AnimatedField
            id="spawn-root-path"
            label="Root path"
            value={form.root_path}
            onChange={(v) => {
              setRootTouched(true)
              setForm((f) => ({ ...f, root_path: v }))
            }}
            placeholder="/workspace"
            required
            hint="Absolute. Seeded from the image, and editable."
          />

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button
              type="submit"
              loading={submitting}
              loadingText="Creating"
              disabled={images.length === 0}
            >
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// Image
// ---------------------------------------------------------------------------

type ImageFormState = {
  name: string
  reference: string
  default_root_path: string
}

const EMPTY_IMAGE_FORM: ImageFormState = {
  name: "",
  reference: "",
  default_root_path: "",
}

export function ImageFormDialog({
  open,
  onOpenChange,
  editing,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  editing: Image | null
  onCreate: (body: CreateImageRequest) => Promise<Image>
  onUpdate: (id: Uuid, patch: UpdateImageRequest) => Promise<Image>
}) {
  const [form, setForm] = React.useState<ImageFormState>(() =>
    editing
      ? {
          name: editing.name,
          reference: editing.reference,
          default_root_path: editing.default_root_path,
        }
      : EMPTY_IMAGE_FORM,
  )
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      const body = {
        name: form.name.trim(),
        reference: form.reference.trim(),
        default_root_path: form.default_root_path.trim(),
      }
      if (editing) await onUpdate(editing.id, body)
      else await onCreate(body)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the image")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{editing ? `Edit ${editing.name}` : "Add image"}</DialogTitle>
          </DialogHeader>

          <AnimatedField
            id="image-name"
            label="Name"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="dev"
            required
          />

          <AnimatedField
            id="image-reference"
            label="Registry reference"
            value={form.reference}
            onChange={(v) => setForm((f) => ({ ...f, reference: v }))}
            placeholder="ghcr.io/acme/dev:latest"
            required
          />

          <AnimatedField
            id="image-root-path"
            label="Default root path"
            value={form.default_root_path}
            onChange={(v) => setForm((f) => ({ ...f, default_root_path: v }))}
            placeholder="/workspace"
            required
            hint="Absolute. Seeds the root path of containers registered from this image."
          />

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="submit" loading={submitting} loadingText={editing ? "Saving" : "Adding"}>
              {editing ? "Save" : "Add image"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
