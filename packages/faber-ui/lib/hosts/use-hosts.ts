"use client"

import * as React from "react"

import {
  faber,
  type CreateContainerRequest,
  type CreateHostRequest,
  type Host,
  type HostContainer,
  type SpawnContainerRequest,
  type UpdateContainerRequest,
  type UpdateHostRequest,
  type Uuid,
} from "@/lib/api"

/**
 * Folds a patched container back into the nested list, which holds active
 * registrations only — the same set the API nests under a host.
 *
 * A patch can move a row either way across that boundary: `unregistered: true`
 * drops it out, and `unregistered: false` brings back one that is not in the
 * list at all, so neither a plain `map` nor a plain `filter` is enough.
 */
function mergeContainer(
  containers: HostContainer[],
  updated: HostContainer,
): HostContainer[] {
  if (updated.unregistered_at) {
    return containers.filter((c) => c.id !== updated.id)
  }
  return containers.some((c) => c.id === updated.id)
    ? containers.map((c) => (c.id === updated.id ? updated : c))
    : [...containers, updated]
}

/**
 * The caller's hosts, with the mutations both host-facing pages need.
 *
 * Deliberately *not* in `AppShellContext`: that context carries sessions and
 * models because the sidebar renders them across every navigation. Hosts only
 * need a static nav entry, so the list is fetched per page instead of kept
 * alive app-wide.
 *
 * Every mutation returns the server's row and folds it back into the list, so
 * `containers` and `last_probe` stay the API's answer rather than a local
 * guess.
 */
export function useHosts() {
  const [hosts, setHosts] = React.useState<Host[]>([])
  const [loaded, setLoaded] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const reload = React.useCallback(async () => {
    const rows = await faber.listHosts()
    setHosts(rows)
    return rows
  }, [])

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listHosts()
      .then((rows) => {
        if (!cancelled) setHosts(rows)
      })
      .catch(() => {
        // An empty list and a failed fetch look the same on screen, so say
        // which one happened rather than showing a bare empty state.
        if (!cancelled) setError("Could not load hosts.")
      })
      .finally(() => {
        if (!cancelled) setLoaded(true)
      })

    return () => {
      cancelled = true
    }
  }, [])

  const addHost = React.useCallback(async (body: CreateHostRequest): Promise<Host> => {
    const created = await faber.createHost(body)
    setHosts((prev) => [...prev, created])
    return created
  }, [])

  const editHost = React.useCallback(
    async (id: Uuid, patch: UpdateHostRequest): Promise<Host> => {
      const updated = await faber.updateHost(id, patch)
      setHosts((prev) => prev.map((host) => (host.id === id ? updated : host)))
      return updated
    },
    [],
  )

  const removeHost = React.useCallback(async (id: Uuid): Promise<void> => {
    await faber.deleteHost(id)
    setHosts((prev) => prev.filter((host) => host.id !== id))
  }, [])

  const addContainer = React.useCallback(
    async (hostId: Uuid, body: CreateContainerRequest): Promise<HostContainer> => {
      const created = await faber.createContainer(hostId, body)
      setHosts((prev) =>
        prev.map((host) =>
          host.id === hostId ? { ...host, containers: [...host.containers, created] } : host,
        ),
      )
      return created
    },
    [],
  )

  /**
   * Starts one from an image, rather than adopting one already running. Folds
   * in identically — the server answers with the registration either way, so
   * nothing downstream has to know which half of the Add menu produced a row.
   */
  const spawnContainer = React.useCallback(
    async (hostId: Uuid, body: SpawnContainerRequest): Promise<HostContainer> => {
      const created = await faber.spawnContainer(hostId, body)
      setHosts((prev) =>
        prev.map((host) =>
          host.id === hostId ? { ...host, containers: [...host.containers, created] } : host,
        ),
      )
      return created
    },
    [],
  )

  const editContainer = React.useCallback(
    async (
      hostId: Uuid,
      id: Uuid,
      patch: UpdateContainerRequest,
    ): Promise<HostContainer> => {
      const updated = await faber.updateContainer(id, patch)
      setHosts((prev) =>
        prev.map((host) =>
          host.id === hostId
            ? { ...host, containers: mergeContainer(host.containers, updated) }
            : host,
        ),
      )
      return updated
    },
    [],
  )

  /**
   * Ends the registration, and destroys the container too when asked.
   */
  const unregisterContainer = React.useCallback(
    async (hostId: Uuid, id: Uuid, destroy = false): Promise<void> => {
      await faber.unregisterContainer(id, { destroy })
      setHosts((prev) =>
        prev.map((host) =>
          host.id === hostId
            ? { ...host, containers: host.containers.filter((c) => c.id !== id) }
            : host,
        ),
      )
    },
    [],
  )

  return {
    hosts,
    loaded,
    error,
    reload,
    addHost,
    editHost,
    removeHost,
    addContainer,
    spawnContainer,
    editContainer,
    unregisterContainer,
  }
}
