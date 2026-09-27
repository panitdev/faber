/**
 * MSW handlers for the whole faber API surface (`lib/api/client.ts`) plus the
 * Surge `/v1` perimeter the auth UI talks to (mounted by the API under
 * `/api/surge`). Paths are relative: `FABER_API_URL` is unset in Storybook, so
 * the client addresses the page's own origin, which is exactly what MSW
 * intercepts.
 *
 * Errors mirror the API's `{ "error": string }` + status convention
 * (`crates/api/src/error.rs`) so `FaberError` handling is exercised as-is.
 *
 * To override one endpoint in a story, prepend a handler in the story's
 * `beforeEach({ msw })` — see `stories/pages.stories.tsx` for examples.
 */

import { delay, http, HttpResponse, type HttpResponseResolver } from "msw"

import type {
  CreateContainerRequest,
  CreateCredentialRequest,
  CreateHostRequest,
  CreateImageRequest,
  CreateModelPresetRequest,
  CreateModelProviderRequest,
  CreateModelRequest,
  CreateSessionRequest,
  CreatedSession,
  Credential,
  Effort,
  EnvironmentCandidate,
  Exchange,
  ExchangeDetail,
  HostContainer,
  Image,
  ModelConfig,
  ModelPreset,
  ModelPresetProvider,
  ModelPresetSpec,
  RecordProbeRequest,
  SendMessageRequest,
  Session,
  SpawnContainerRequest,
  ThinkingCapability,
  Thread,
  UpdateContainerRequest,
  UpdateHostRequest,
  UpdateImageRequest,
  UpdateModelPresetRequest,
  UpdateModelProviderRequest,
  UpdateModelRequest,
  UpdateSessionRequest,
  Uuid,
} from "@/lib/api"

import { interrupt, isRunActive, startRun, subscribe } from "./agent"
import { composeHost, db, mockSettings, nowEpoch, nowIso, uuid, type HostRow } from "./db"

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function fail(status: number, error: string) {
  return HttpResponse.json({ error }, { status })
}

const noContent = () => new HttpResponse(null, { status: 204 })

/** Every REST answer waits `latency` first, so loading states are visible. */
function slow<P extends Record<string, string>>(
  resolver: (args: Parameters<HttpResponseResolver<P>>[0]) => Response | Promise<Response>,
): HttpResponseResolver<P> {
  return async (args) => {
    await delay(mockSettings().latency)
    return resolver(args)
  }
}

/** Signed-out requests get the 401 the API would give them. */
function authed<P extends Record<string, string>>(
  resolver: (args: Parameters<HttpResponseResolver<P>>[0]) => Response | Promise<Response>,
): HttpResponseResolver<P> {
  return slow<P>((args) => (db().surgeSession ? resolver(args) : fail(401, "missing or expired session")))
}

async function body<T>(request: Request): Promise<T> {
  const text = await request.text()
  return (text ? JSON.parse(text) : {}) as T
}

/** Applies a PATCH body: `undefined` members leave the field alone, `null` clears it. */
function patch<T extends object>(row: T, changes: Partial<T>) {
  for (const [key, value] of Object.entries(changes)) {
    if (value !== undefined) (row as Record<string, unknown>)[key] = value
  }
}

const EMPTY_SPEC: ModelPresetSpec = {
  provider: "",
  provider_name: "",
  id: "",
  base_model: null,
  name: "",
  description: null,
  family: null,
  attachment: false,
  reasoning: false,
  tool_call: false,
  structured_output: null,
  temperature: null,
  knowledge: null,
  release_date: null,
  last_updated: null,
  open_weights: null,
  limit: { context: null, input: null, output: null },
  modalities: { input: [], output: [] },
  cost: null,
  reasoning_options: null,
  interleaved: null,
  status: null,
}

const SPEC_KEYS = [
  "name",
  "description",
  "family",
  "attachment",
  "reasoning",
  "tool_call",
  "structured_output",
  "temperature",
  "knowledge",
  "release_date",
  "last_updated",
  "open_weights",
  "limit",
  "modalities",
] as const

/**
 * Lays a stored preset's overrides over its creator model, in place — what
 * the API does on every read. Without a creator model the empty description
 * is the base, and a name nobody states falls back to the served id.
 */
function resolveRow(row: ModelPreset) {
  const base = db().creatorModels.find((model) => model.creator_model_id === row.creator_model_id)
  row.base_model = base?.id ?? null
  for (const key of SPEC_KEYS) {
    const override = row.overrides[key]
    const inherited = base ? base[key] : EMPTY_SPEC[key]
    Object.assign(row, { [key]: override ?? inherited })
  }
  if (!row.name) row.name = row.id
}

const EFFORT_ORDER: Effort[] = ["minimal", "low", "medium", "high", "xhigh", "max"]

/**
 * The knob a model is read against, by the API's rule: `params.thinking` when
 * the row states one, else what its preset's `reasoning_options` offer — any
 * control turns it on, an `effort` control's known values are the levels.
 */
function effectiveThinking(model: ModelConfig): ThinkingCapability {
  const params = model.params as Record<string, unknown> | null
  const own = params && typeof params === "object" ? params.thinking : undefined
  if (own !== undefined && own !== null) return own as ThinkingCapability

  const options = model.preset.reasoning_options
  if (!model.preset.reasoning || !Array.isArray(options) || options.length === 0) {
    return { supported: false, efforts: [] }
  }
  const offered = options.flatMap((option) => {
    const o = option as { type?: string; values?: unknown[] }
    return o.type === "effort" && Array.isArray(o.values) ? o.values : []
  })
  return {
    supported: true,
    efforts: EFFORT_ORDER.filter((effort) => offered.includes(effort)),
  }
}

/** A model as the API returns it, its thinking knob resolved. */
function modelResponse(model: ModelConfig): ModelConfig {
  return { ...model, thinking: effectiveThinking(model) }
}

function resolvePreset(presetId: Uuid | null | undefined): ModelPresetSpec {
  if (!presetId) return EMPTY_SPEC
  const found = db().presets.find((preset) => preset.preset_id === presetId)
  if (!found) return EMPTY_SPEC
  const specOnly: Partial<ModelPreset> = { ...found }
  delete specOnly.preset_id
  delete specOnly.owned
  delete specOnly.created_at
  delete specOnly.model_provider_id
  delete specOnly.creator_model_id
  delete specOnly.overrides
  return specOnly as ModelPresetSpec
}

function recountProviders() {
  for (const provider of db().providers) {
    provider.model_count = db().presets.filter(
      (preset) => preset.model_provider_id === provider.provider_id,
    ).length
  }
}

/** Fills in every override a request leaves out as "as the base says". */
function fullOverrides(overrides: CreateModelPresetRequest["overrides"]): ModelPreset["overrides"] {
  const full = {} as ModelPreset["overrides"]
  for (const key of SPEC_KEYS) Object.assign(full, { [key]: overrides?.[key] ?? null })
  return full
}

function environments(): EnvironmentCandidate[] {
  const { hosts, containers } = db()
  const candidates: EnvironmentCandidate[] = []

  for (const host of hosts) {
    if (host.root_path) {
      candidates.push({
        label: host.name,
        kind: "host",
        host_id: host.id,
        host_name: host.name,
        container_id: null,
        root_path: host.root_path,
        disabled: host.disabled_at !== null,
        bind_by_default: host.bind_by_default,
      })
    }
  }

  for (const container of containers) {
    if (container.unregistered_at !== null) continue
    const host = hosts.find((row) => row.id === container.host_id)
    if (!host) continue
    candidates.push({
      label: container.name ?? container.container_ref.slice(0, 12),
      kind: "container",
      host_id: host.id,
      host_name: host.name,
      container_id: container.id,
      root_path: container.root_path,
      disabled: host.disabled_at !== null,
      bind_by_default: container.bind_by_default,
    })
  }

  // Qualify as `host/name` only where two would otherwise collide.
  const counts = new Map<string, number>()
  for (const candidate of candidates) counts.set(candidate.label, (counts.get(candidate.label) ?? 0) + 1)
  return candidates.map((candidate) =>
    (counts.get(candidate.label) ?? 0) > 1 && candidate.kind === "container"
      ? { ...candidate, label: `${candidate.host_name}/${candidate.label}` }
      : candidate,
  )
}

/** Binds a candidate to a session unless the label is already claimed there. */
function bind(sessionId: Uuid, candidate: EnvironmentCandidate): string | null {
  const bound = (db().sessionEnvironments[sessionId] ??= [])
  if (bound.some((row) => row.label === candidate.label)) return null
  bound.push({
    label: candidate.label,
    host_id: candidate.host_id,
    container_id: candidate.container_id,
    added_at: nowEpoch(),
    removed_at: null,
  })
  return candidate.label
}

function noticeFor(candidate: EnvironmentCandidate): string {
  return candidate.kind === "container"
    ? `Added @${candidate.label} — container on ${candidate.host_name}, rooted at ${candidate.root_path}.`
    : `Added @${candidate.label} — host ${candidate.host_name}, rooted at ${candidate.root_path}.`
}

function hostRow(id: string): HostRow | undefined {
  return db().hosts.find((row) => row.id === id)
}

// ---------------------------------------------------------------------------
// SSE
// ---------------------------------------------------------------------------

const encoder = new TextEncoder()

function sseFrame(event: string, data: unknown) {
  return encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`)
}

/**
 * `GET /api/sessions/:id/stream`. Replays the run named by the resume cursor
 * past `after_seq`, then stays open and forwards live events — the same
 * contract as the API, so reconnects leave no gap.
 */
const streamSession: HttpResponseResolver<{ id: string }> = ({ params, request }) => {
  if (!db().surgeSession) return fail(401, "missing or expired session")
  const sessionId = params.id
  if (!db().sessions.some((row) => row.id === sessionId)) return fail(404, "session not found")

  const url = new URL(request.url)
  const runId = url.searchParams.get("run_id")
  const afterSeqRaw = url.searchParams.get("after_seq")
  if ((runId === null) !== (afterSeqRaw === null)) {
    return fail(400, "run_id and after_seq must be given together")
  }

  let cleanup = () => {}

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      let closed = false
      const send = (chunk: Uint8Array) => {
        if (closed) return
        try {
          controller.enqueue(chunk)
        } catch {
          cleanup()
        }
      }

      if (runId !== null && afterSeqRaw !== null) {
        const afterSeq = Number(afterSeqRaw)
        for (const event of db().transcripts[runId] ?? []) {
          if (event.seq > afterSeq) {
            send(sseFrame("transcript", { run_id: runId, seq: event.seq, kind: event.kind, payload: event.payload }))
          }
        }
      }

      const unsubscribe = subscribe(sessionId, (event) => send(sseFrame("transcript", event)))
      const keepAlive = setInterval(() => send(encoder.encode(": ping\n\n")), 15_000)

      cleanup = () => {
        if (closed) return
        closed = true
        unsubscribe()
        clearInterval(keepAlive)
        try {
          controller.close()
        } catch {
          // Already closed by the reader.
        }
      }
      request.signal.addEventListener("abort", () => cleanup(), { once: true })
    },
    cancel() {
      cleanup()
    },
  })

  return new HttpResponse(stream, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    },
  })
}

// ---------------------------------------------------------------------------
// Surge perimeter (`/api/surge/v1`)
// ---------------------------------------------------------------------------

const SURGE = "/api/surge/v1"
const FLOW_ID = "aeg_f_storybook"

function signIn(username: string, displayName?: string) {
  const now = nowIso()
  db().surgeSession = {
    id: "aeg_s_storybook",
    identity: {
      id: "00000000-0000-4000-8000-0000000000aa",
      username: username.toLowerCase() || "geon",
      display_name: displayName || username || "Geon",
      avatar_url: null,
      state: "active",
      created_at: now,
      updated_at: now,
    },
    issued_at: now,
    expires_at: new Date(Date.now() + 30 * 24 * 3600 * 1000).toISOString(),
    authenticated_via: "password",
    policy: {
      required: { totp: false, passphrase: false },
      has: { totp: false, passphrase: false },
      compliant: true,
    },
  }
  const session = db().surgeSession!
  return { return_to: null, session, policy: session.policy }
}

const surgeHandlers = [
  http.get(`${SURGE}/whoami`, slow(() => {
    const session = db().surgeSession
    return session ? HttpResponse.json(session) : fail(401, "unauthenticated")
  })),
  http.post(`${SURGE}/logout`, slow(() => {
    db().surgeSession = null
    return noContent()
  })),
  http.get(`${SURGE}/login`, slow(() =>
    HttpResponse.json({ flow_id: FLOW_ID, csrf_token: "csrf-storybook", registration_mode: "open" }),
  )),
  http.get(`${SURGE}/flows/:id`, slow(({ params }) =>
    HttpResponse.json({
      id: params.id,
      state: "created",
      csrf_token: "csrf-storybook",
      error: null,
      registration_enabled: true,
    }),
  )),
  // Any username/password signs in, except the password "wrong", which lets a
  // story show the error path.
  http.post(`${SURGE}/flows/:id/password`, slow(async ({ request }) => {
    const { username, password } = await body<{ username: string; password: string }>(request)
    if (password === "wrong") return fail(401, "invalid_credentials")
    return HttpResponse.json(signIn(username))
  })),
  http.post(`${SURGE}/flows/:id/register`, slow(async ({ request }) => {
    const { username, display_name } = await body<{ username: string; display_name: string }>(request)
    return HttpResponse.json(signIn(username, display_name))
  })),
  http.get(`${SURGE}/factors`, authed(() =>
    HttpResponse.json({ policy: db().surgeSession!.policy }),
  )),
]

// ---------------------------------------------------------------------------
// Faber API
// ---------------------------------------------------------------------------

const identityHandlers = [
  http.get("/health", slow(() => HttpResponse.json({ ok: true }))),
  http.get("/api/me", authed(() => HttpResponse.json(db().me))),
  http.post("/api/logout", slow(() => {
    db().surgeSession = null
    return noContent()
  })),
  http.get("/api/config", authed(() => HttpResponse.json(db().config))),
  http.get("/api/workspaces", authed(() => HttpResponse.json(db().workspaces))),
]

const credentialHandlers = [
  http.get("/api/credentials", authed(({ request }) => {
    const kind = new URL(request.url).searchParams.get("kind")
    return HttpResponse.json(db().credentials.filter((row) => !kind || row.kind === kind))
  })),
  http.post("/api/credentials", authed(async ({ request }) => {
    const input = await body<CreateCredentialRequest>(request)
    if (!input.label?.trim()) return fail(400, "label must not be empty")
    if (!input.key?.trim()) return fail(400, "key must not be empty")
    if (db().credentials.some((row) => row.label === input.label)) {
      return fail(400, `a credential named "${input.label}" already exists`)
    }
    const row: Credential = {
      id: uuid(),
      label: input.label,
      kind: input.kind,
      last_four: input.key.slice(-4),
      created_at: nowIso(),
    }
    db().credentials.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),
  http.delete("/api/credentials/:id", authed(({ params }) => {
    const before = db().credentials.length
    db().credentials = db().credentials.filter((row) => row.id !== params.id)
    return before === db().credentials.length ? fail(404, "credential not found") : noContent()
  })),
]

const modelHandlers = [
  http.get("/api/models", authed(() => HttpResponse.json(db().models.map(modelResponse)))),
  http.post("/api/models", authed(async ({ request }) => {
    const input = await body<CreateModelRequest>(request)
    if (db().models.some((row) => row.alias === input.alias)) {
      return fail(400, `a model with alias "${input.alias}" already exists`)
    }
    if (input.credential_id && !db().credentials.some((row) => row.id === input.credential_id)) {
      return fail(400, "credential not found")
    }
    const row: ModelConfig = {
      id: uuid(),
      alias: input.alias,
      base_url: input.base_url,
      wire: input.wire,
      wire_id: input.wire_id,
      family: input.family ?? null,
      credential_id: input.credential_id ?? null,
      params: input.params ?? {},
      preset_id: input.preset_id ?? null,
      preset: resolvePreset(input.preset_id),
      created_at: nowIso(),
    }
    db().models.push(row)
    return HttpResponse.json(modelResponse(row), { status: 201 })
  })),
  http.patch("/api/models/:id", authed(async ({ params, request }) => {
    const row = db().models.find((model) => model.id === params.id)
    if (!row) return fail(404, "model not found")
    const changes = await body<UpdateModelRequest>(request)
    patch(row, changes)
    if ("preset_id" in changes) row.preset = resolvePreset(row.preset_id)
    return HttpResponse.json(modelResponse(row))
  })),
  http.delete("/api/models/:id", authed(({ params }) => {
    db().models = db().models.filter((row) => row.id !== params.id)
    return noContent()
  })),

  http.get("/api/creator-models", authed(({ request }) => {
    const search = new URL(request.url).searchParams
    const flag = (key: string) => (search.has(key) ? search.get(key) === "true" : undefined)
    const q = search.get("q")?.toLowerCase()
    const creator = search.get("creator")
    const vision = flag("vision")
    const reasoning = flag("reasoning")
    const toolCall = flag("tool_call")
    const limit = Math.min(Math.max(Number(search.get("limit") ?? 100), 1), 500)
    const offset = Number(search.get("offset") ?? 0)

    const filtered = db()
      .creatorModels.filter(
        (model) =>
          (!q || `${model.name} ${model.id}`.toLowerCase().includes(q)) &&
          (!creator || model.creator === creator) &&
          (vision === undefined || model.modalities.input.includes("image") === vision) &&
          (reasoning === undefined || model.reasoning === reasoning) &&
          (toolCall === undefined || model.tool_call === toolCall),
      )
      .map((model) => ({
        ...model,
        preset_count: db().presets.filter(
          (preset) => preset.creator_model_id === model.creator_model_id,
        ).length,
      }))
    return HttpResponse.json({ total: filtered.length, limit, offset, items: filtered.slice(offset, offset + limit) })
  })),
  http.get("/api/creator-models/:id", authed(({ params }) => {
    const row = db().creatorModels.find((model) => model.creator_model_id === params.id)
    return row ? HttpResponse.json(row) : fail(404, "creator model not found")
  })),

  http.get("/api/model-presets", authed(({ request }) => {
    const search = new URL(request.url).searchParams
    const flag = (key: string) => (search.has(key) ? search.get(key) === "true" : undefined)
    const q = search.get("q")?.toLowerCase()
    const providerKey = search.get("provider")
    const baseModel = search.get("base_model")
    const modelProviderId = search.get("model_provider_id")
    const owned = flag("owned")
    const vision = flag("vision")
    const reasoning = flag("reasoning")
    const toolCall = flag("tool_call")
    const limit = Math.min(Math.max(Number(search.get("limit") ?? 100), 1), 500)
    const offset = Number(search.get("offset") ?? 0)

    const filtered = db().presets.filter(
      (preset) =>
        (!q ||
          `${preset.name} ${preset.id} ${preset.provider} ${preset.provider_name} ${preset.base_model ?? ""}`
            .toLowerCase()
            .includes(q)) &&
        (!providerKey || preset.provider === providerKey) &&
        (!baseModel || preset.base_model === baseModel) &&
        (!modelProviderId || preset.model_provider_id === modelProviderId) &&
        (owned === undefined || preset.owned === owned) &&
        (vision === undefined || preset.modalities.input.includes("image") === vision) &&
        (reasoning === undefined || preset.reasoning === reasoning) &&
        (toolCall === undefined || preset.tool_call === toolCall),
    )
    return HttpResponse.json({ total: filtered.length, limit, offset, items: filtered.slice(offset, offset + limit) })
  })),
  http.get("/api/model-presets/:id", authed(({ params }) => {
    const row = db().presets.find((preset) => preset.preset_id === params.id)
    return row ? HttpResponse.json(row) : fail(404, "preset not found")
  })),
  http.post("/api/model-presets", authed(async ({ request }) => {
    const input = await body<CreateModelPresetRequest>(request)
    const owner = db().providers.find((row) => row.provider_id === input.provider_id)
    if (!owner) return fail(400, "provider not found")
    if (!owner.owned) return fail(403, "that provider belongs to the system")
    if (
      input.creator_model_id &&
      !db().creatorModels.some((model) => model.creator_model_id === input.creator_model_id)
    ) {
      return fail(400, "creator model not found")
    }
    const row: ModelPreset = {
      ...EMPTY_SPEC,
      preset_id: uuid(),
      owned: true,
      created_at: nowIso(),
      model_provider_id: owner.provider_id,
      creator_model_id: input.creator_model_id ?? null,
      overrides: fullOverrides(input.overrides),
      provider: owner.id,
      provider_name: owner.name,
      id: input.id,
      cost: (input.cost as ModelPreset["cost"]) ?? null,
      reasoning_options: input.reasoning_options ?? null,
      status: input.status ?? null,
    }
    resolveRow(row)
    db().presets.push(row)
    recountProviders()
    return HttpResponse.json(row, { status: 201 })
  })),
  http.patch("/api/model-presets/:id", authed(async ({ params, request }) => {
    const row = db().presets.find((preset) => preset.preset_id === params.id)
    if (!row) return fail(404, "preset not found")
    if (!row.owned) return fail(403, "system presets cannot be changed")
    const input = await body<UpdateModelPresetRequest>(request)
    if (input.provider_id) {
      const owner = db().providers.find((provider) => provider.provider_id === input.provider_id)
      if (!owner?.owned) return fail(400, "provider not found")
      row.model_provider_id = owner.provider_id
      row.provider = owner.id
      row.provider_name = owner.name
    }
    if (input.creator_model_id !== undefined) {
      if (
        input.creator_model_id &&
        !db().creatorModels.some((model) => model.creator_model_id === input.creator_model_id)
      ) {
        return fail(400, "creator model not found")
      }
      row.creator_model_id = input.creator_model_id
    }
    if (input.id !== undefined) row.id = input.id
    // Replaced whole, as the API does: an override left out goes back to the base's.
    if (input.overrides !== undefined) row.overrides = fullOverrides(input.overrides)
    if (input.cost !== undefined) row.cost = (input.cost as ModelPreset["cost"]) ?? null
    if (input.reasoning_options !== undefined) row.reasoning_options = input.reasoning_options
    if (input.status !== undefined) row.status = input.status
    resolveRow(row)
    recountProviders()
    return HttpResponse.json(row)
  })),
  http.delete("/api/model-presets/:id", authed(({ params }) => {
    const row = db().presets.find((preset) => preset.preset_id === params.id)
    if (!row) return fail(404, "preset not found")
    if (!row.owned) return fail(403, "system presets cannot be deleted")
    db().presets = db().presets.filter((preset) => preset !== row)
    recountProviders()
    return noContent()
  })),

  http.get("/api/model-providers", authed(() => HttpResponse.json(db().providers))),
  http.get("/api/model-providers/:id", authed(({ params }) => {
    const row = db().providers.find((provider) => provider.provider_id === params.id)
    return row ? HttpResponse.json(row) : fail(404, "provider not found")
  })),
  http.post("/api/model-providers", authed(async ({ request }) => {
    const input = await body<CreateModelProviderRequest>(request)
    if (db().providers.some((row) => row.owned && row.id === input.id)) {
      return fail(400, `a provider with key "${input.id}" already exists`)
    }
    const row: ModelPresetProvider = {
      provider_id: uuid(),
      id: input.id,
      name: input.name,
      doc: input.doc ?? null,
      api: input.api ?? null,
      npm: input.npm ?? null,
      env: input.env ?? [],
      model_count: 0,
      owned: true,
      created_at: nowIso(),
    }
    db().providers.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),
  http.patch("/api/model-providers/:id", authed(async ({ params, request }) => {
    const row = db().providers.find((provider) => provider.provider_id === params.id)
    if (!row) return fail(404, "provider not found")
    if (!row.owned) return fail(403, "system providers cannot be changed")
    patch(row, await body<UpdateModelProviderRequest>(request))
    return HttpResponse.json(row)
  })),
  http.delete("/api/model-providers/:id", authed(({ params }) => {
    const row = db().providers.find((provider) => provider.provider_id === params.id)
    if (!row) return fail(404, "provider not found")
    if (!row.owned) return fail(403, "system providers cannot be deleted")
    db().providers = db().providers.filter((provider) => provider !== row)
    db().presets = db().presets.filter((preset) => preset.model_provider_id !== row.provider_id)
    return noContent()
  })),
]

const hostHandlers = [
  http.get("/api/hosts", authed(() => HttpResponse.json(db().hosts.map(composeHost)))),
  http.post("/api/hosts", authed(async ({ request }) => {
    const input = await body<CreateHostRequest>(request)
    if (!input.name?.trim()) return fail(400, "name must not be empty")
    if (input.transport === "ssh" && !input.ssh_address) return fail(400, "ssh hosts need an ssh_address")
    if (input.transport === "local" && !db().config.allow_local_hosts) {
      return fail(400, "local hosts are disabled on this server")
    }
    if (db().hosts.some((row) => row.name === input.name)) return fail(400, `a host named "${input.name}" already exists`)
    const row: HostRow = {
      id: uuid(),
      name: input.name,
      transport: input.transport,
      exec_mode: input.exec_mode,
      ssh_address: input.transport === "ssh" ? input.ssh_address ?? null : null,
      ssh_key_ref: input.ssh_key_ref ?? null,
      docker_endpoint: input.docker_endpoint ?? null,
      root_path: input.root_path ?? null,
      created_at: nowIso(),
      disabled_at: null,
      bind_by_default: input.bind_by_default ?? false,
    }
    db().hosts.push(row)
    if (row.transport === "agent") db().agents[row.id] = { connected: false, enrolled_at: null }
    return HttpResponse.json(composeHost(row), { status: 201 })
  })),
  http.get("/api/hosts/:id", authed(({ params }) => {
    const row = hostRow(params.id)
    return row ? HttpResponse.json(composeHost(row)) : fail(404, "host not found")
  })),
  http.patch("/api/hosts/:id", authed(async ({ params, request }) => {
    const row = hostRow(params.id)
    if (!row) return fail(404, "host not found")
    const { disabled, ...changes } = await body<UpdateHostRequest>(request)
    patch(row, changes)
    if (disabled !== undefined) row.disabled_at = disabled ? nowIso() : null
    return HttpResponse.json(composeHost(row))
  })),
  http.delete("/api/hosts/:id", authed(({ params }) => {
    const store = db()
    store.hosts = store.hosts.filter((row) => row.id !== params.id)
    store.containers = store.containers.filter((row) => row.host_id !== params.id)
    store.probes = store.probes.filter((row) => row.host_id !== params.id)
    delete store.agents[params.id]
    return noContent()
  })),

  // Agent enrollment. The daemon "connects" a few seconds after the command
  // is issued, so the waiting state is visible and then resolves on its own.
  http.post("/api/hosts/:id/agent/enroll", authed(({ params }) => {
    const row = hostRow(params.id)
    if (!row) return fail(404, "host not found")
    if (row.transport !== "agent") return fail(400, "only agent hosts can be enrolled")
    const token = `fbr_boot_${uuid().replace(/-/g, "").slice(0, 24)}`
    const hostId = row.id
    const database = db()
    setTimeout(() => {
      if (database !== db()) return
      db().agents[hostId] = { connected: true, enrolled_at: nowIso() }
    }, 4000)
    return HttpResponse.json({
      token,
      expires_at: new Date(Date.now() + 15 * 60 * 1000).toISOString(),
      install_command: `curl -fsSL ${location.origin}/install.sh | sh -s -- --token ${token}`,
    })
  })),
  http.get("/api/hosts/:id/agent", authed(({ params }) => {
    if (!hostRow(params.id)) return fail(404, "host not found")
    return HttpResponse.json(db().agents[params.id] ?? { connected: false, enrolled_at: null })
  })),

  http.get("/api/hosts/:id/containers", authed(({ params, request }) => {
    const all = new URL(request.url).searchParams.get("include_unregistered") === "true"
    return HttpResponse.json(
      db().containers.filter((row) => row.host_id === params.id && (all || row.unregistered_at === null)),
    )
  })),
  http.post("/api/hosts/:id/containers", authed(async ({ params, request }) => {
    const host = hostRow(params.id)
    if (!host) return fail(404, "host not found")
    if (host.exec_mode !== "docker") return fail(400, "containers need a docker-mode host")
    const input = await body<CreateContainerRequest>(request)
    if (!input.root_path?.startsWith("/")) return fail(400, "root_path must be absolute")
    const row: HostContainer = {
      id: uuid(),
      host_id: host.id,
      container_ref: input.container_ref,
      name: input.name ?? null,
      root_path: input.root_path,
      created_at: nowIso(),
      unregistered_at: null,
      bind_by_default: input.bind_by_default ?? false,
      managed: false,
      managed_at: null,
      image_id: null,
    }
    db().containers.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),
  http.post("/api/hosts/:id/containers/spawn", authed(async ({ params, request }) => {
    const host = hostRow(params.id)
    if (!host) return fail(404, "host not found")
    if (host.exec_mode !== "docker") return fail(400, "containers need a docker-mode host")
    const input = await body<SpawnContainerRequest>(request)
    const image = db().images.find((row) => row.id === input.image_id)
    if (!image) return fail(400, "image not found")
    await delay(800) // pulling + starting
    const row: HostContainer = {
      id: uuid(),
      host_id: host.id,
      container_ref: input.name || `faber-${uuid().slice(0, 8)}`,
      name: input.name ?? null,
      root_path: input.root_path ?? image.default_root_path,
      created_at: nowIso(),
      unregistered_at: null,
      bind_by_default: false,
      managed: true,
      managed_at: nowIso(),
      image_id: image.id,
    }
    db().containers.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),
  http.patch("/api/host-containers/:id", authed(async ({ params, request }) => {
    const row = db().containers.find((container) => container.id === params.id)
    if (!row) return fail(404, "container not found")
    const { unregistered, ...changes } = await body<UpdateContainerRequest>(request)
    patch(row, changes)
    if (unregistered !== undefined) row.unregistered_at = unregistered ? nowIso() : null
    return HttpResponse.json(row)
  })),
  http.delete("/api/host-containers/:id", authed(({ params, request }) => {
    const row = db().containers.find((container) => container.id === params.id)
    if (!row) return fail(404, "container not found")
    const destroy = new URL(request.url).searchParams.get("destroy") === "true"
    if (destroy && !row.managed) return fail(400, "faber did not create this container, so it will not destroy it")
    if (destroy) db().containers = db().containers.filter((container) => container !== row)
    else row.unregistered_at = nowIso()
    return noContent()
  })),

  http.get("/api/hosts/:id/probes", authed(({ params, request }) => {
    const limit = Number(new URL(request.url).searchParams.get("limit") ?? 50)
    return HttpResponse.json(
      db()
        .probes.filter((row) => row.host_id === params.id)
        .sort((a, b) => b.probed_at.localeCompare(a.probed_at))
        .slice(0, limit),
    )
  })),
  http.post("/api/hosts/:id/probes", authed(async ({ params, request }) => {
    if (!hostRow(params.id)) return fail(404, "host not found")
    const input = await body<RecordProbeRequest>(request)
    const row = {
      id: uuid(),
      host_id: params.id,
      container_id: input.container_id ?? null,
      probed_at: nowIso(),
      ok: input.ok,
      error: input.error ?? null,
      os: input.os ?? null,
      arch: input.arch ?? null,
      shell: input.shell ?? null,
      tools: input.tools ?? null,
      root_path: input.root_path ?? null,
    }
    db().probes.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),

  http.get("/api/images", authed(() => HttpResponse.json(db().images))),
  http.post("/api/images", authed(async ({ request }) => {
    const input = await body<CreateImageRequest>(request)
    if (!input.default_root_path?.startsWith("/")) return fail(400, "default_root_path must be absolute")
    const row: Image = {
      id: uuid(),
      name: input.name,
      reference: input.reference,
      default_mounts: input.default_mounts ?? null,
      default_root_path: input.default_root_path,
      created_at: nowIso(),
    }
    db().images.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),
  http.patch("/api/images/:id", authed(async ({ params, request }) => {
    const row = db().images.find((image) => image.id === params.id)
    if (!row) return fail(404, "image not found")
    patch(row, await body<UpdateImageRequest>(request))
    return HttpResponse.json(row)
  })),
  http.delete("/api/images/:id", authed(({ params }) => {
    db().images = db().images.filter((row) => row.id !== params.id)
    return noContent()
  })),

  http.get("/api/environments", authed(() => HttpResponse.json(environments()))),
]

const sessionHandlers = [
  http.get("/api/sessions", authed(({ request }) => {
    const search = new URL(request.url).searchParams
    const workspace = search.get("workspace_id")
    const limit = Number(search.get("limit") ?? 100)
    const rows = db()
      .sessions.filter((row) => !workspace || row.workspace_id === workspace)
      .sort((a, b) => b.created_at - a.created_at)
      .slice(0, limit)
    return HttpResponse.json(rows)
  })),
  http.post("/api/sessions", authed(async ({ request }) => {
    const input = await body<CreateSessionRequest>(request)
    const createdAt = nowEpoch()
    const session: Session = {
      id: uuid(),
      workspace_id: input.workspace_id ?? db().workspaces[0].id,
      title: input.title ?? null,
      created_at: createdAt,
      closed_at: null,
      model: null,
      thinking_effort: null,
    }
    const thread: Thread = {
      id: uuid(),
      session_id: session.id,
      parent_id: null,
      forked_at_seq: null,
      next_seq: 0,
      created_at: createdAt,
    }
    db().sessions.unshift(session)
    db().threads.push(thread)
    const defaults = environments()
      .filter((candidate) => candidate.bind_by_default && !candidate.disabled)
      .map((candidate) => bind(session.id, candidate))
      .filter((label): label is string => label !== null)
    const created: CreatedSession = { ...session, root_thread: thread, default_environments: defaults }
    return HttpResponse.json(created, { status: 201 })
  })),
  http.get("/api/sessions/:id", authed(({ params }) => {
    const row = db().sessions.find((session) => session.id === params.id)
    return row ? HttpResponse.json(row) : fail(404, "session not found")
  })),
  http.patch("/api/sessions/:id", authed(async ({ params, request }) => {
    const row = db().sessions.find((session) => session.id === params.id)
    if (!row) return fail(404, "session not found")
    const { closed, ...changes } = await body<UpdateSessionRequest>(request)
    if (changes.model && !db().models.some((model) => model.alias === changes.model)) {
      return fail(400, `no model with alias "${changes.model}"`)
    }
    patch(row, changes)
    if (closed !== undefined) row.closed_at = closed ? nowEpoch() : null
    return HttpResponse.json(row)
  })),
  http.delete("/api/sessions/:id", authed(({ params }) => {
    const store = db()
    const threadIds = new Set(store.threads.filter((row) => row.session_id === params.id).map((row) => row.id))
    const runIds = store.runs.filter((row) => threadIds.has(row.thread_id)).map((row) => row.id)
    store.sessions = store.sessions.filter((row) => row.id !== params.id)
    store.threads = store.threads.filter((row) => !threadIds.has(row.id))
    store.runs = store.runs.filter((row) => !threadIds.has(row.thread_id))
    for (const id of runIds) delete store.transcripts[id]
    delete store.sessionEnvironments[params.id]
    return noContent()
  })),

  http.get("/api/sessions/:id/threads", authed(({ params }) =>
    HttpResponse.json(
      db()
        .threads.filter((row) => row.session_id === params.id)
        .sort((a, b) => a.created_at - b.created_at),
    ),
  )),
  http.post("/api/sessions/:id/threads", authed(({ params }) => {
    const row: Thread = {
      id: uuid(),
      session_id: params.id,
      parent_id: null,
      forked_at_seq: null,
      next_seq: 0,
      created_at: nowEpoch(),
    }
    db().threads.push(row)
    return HttpResponse.json(row, { status: 201 })
  })),

  http.get("/api/sessions/:id/environments", authed(({ params }) =>
    HttpResponse.json(db().sessionEnvironments[params.id] ?? []),
  )),
  http.delete("/api/sessions/:id/environments/:label", authed(({ params }) => {
    const row = (db().sessionEnvironments[params.id] ?? []).find(
      (binding) => binding.label === params.label && binding.removed_at === null,
    )
    if (!row) return fail(404, "not bound")
    row.removed_at = nowEpoch()
    return noContent()
  })),

  http.post("/api/sessions/:id/messages", authed(async ({ params, request }) => {
    const session = db().sessions.find((row) => row.id === params.id)
    if (!session) return fail(404, "session not found")
    const input = await body<SendMessageRequest>(request)
    if (!input.content?.trim()) return fail(400, "content must not be empty")

    if (input.model) {
      if (!db().models.some((row) => row.alias === input.model)) return fail(400, `no model with alias "${input.model}"`)
      session.model = input.model
    }
    if (input.thinking_effort) session.thinking_effort = input.thinking_effort
    if (!session.model) return fail(400, "this session has no model; pick one before sending")

    const threads = db().threads.filter((row) => row.session_id === session.id)
    const thread = input.thread_id ? threads.find((row) => row.id === input.thread_id) : threads[0]
    if (!thread) return fail(400, "thread_id is required once a session has more than one thread")

    const busy = db().runs.some((run) => run.thread_id === thread.id && isRunActive(run.id))
    if (busy) return fail(409, "a run is already in progress on this thread")

    // `@label` tags bind environments, and the session says so in the transcript.
    const candidates = environments()
    const added: string[] = []
    const notices: string[] = []
    for (const [, label] of input.content.matchAll(/@([\w./-]+)/g)) {
      const candidate = candidates.find((row) => row.label === label)
      if (!candidate) continue
      if (candidate.disabled) return fail(400, `@${label} is on a disabled host`)
      if (bind(session.id, candidate)) {
        added.push(label)
        notices.push(noticeFor(candidate))
      }
    }

    const run = startRun({
      sessionId: session.id,
      threadId: thread.id,
      content: input.content,
      model: session.model,
      notices,
    })
    return HttpResponse.json(
      { run_id: run.id, thread_id: thread.id, added_environments: added },
      { status: 202 },
    )
  })),

  http.get("/api/sessions/:id/stream", streamSession),

  http.get("/api/threads/:id", authed(({ params }) => {
    const row = db().threads.find((thread) => thread.id === params.id)
    return row ? HttpResponse.json(row) : fail(404, "thread not found")
  })),
  http.get("/api/threads/:id/spine", authed(({ params }) =>
    HttpResponse.json(
      db()
        .runs.filter((run) => run.thread_id === params.id && run.completed_at !== null)
        .map((run, seq) => ({ seq, exchange_id: run.id, explicit_commit: false, created_at: run.created_at })),
    ),
  )),
  http.get("/api/threads/:id/runs", authed(({ params }) =>
    HttpResponse.json(
      db()
        .runs.filter((run) => run.thread_id === params.id)
        .sort((a, b) => a.created_at - b.created_at),
    ),
  )),

  http.get("/api/runs/:id/transcript", authed(({ params, request }) => {
    const search = new URL(request.url).searchParams
    const afterSeq = search.has("after_seq") ? Number(search.get("after_seq")) : -Infinity
    const limit = Number(search.get("limit") ?? 500)
    const events = db().transcripts[params.id]
    if (!events) return fail(404, "run not found")
    return HttpResponse.json(events.filter((event) => event.seq > afterSeq).slice(0, limit))
  })),

  // The debug viewer's view of provider calls: one exchange per assistant
  // message, with a request blob shaped like what a provider would have seen.
  http.get("/api/runs/:id/exchanges", authed(({ params }) => {
    const run = db().runs.find((row) => row.id === params.id)
    if (!run) return fail(404, "run not found")
    return HttpResponse.json(exchangesFor(params.id))
  })),
  http.get("/api/exchanges/:id", authed(({ params }) => {
    for (const run of db().runs) {
      const found = exchangesFor(run.id).find((exchange) => exchange.id === params.id)
      if (found) return HttpResponse.json(found)
    }
    return fail(404, "exchange not found")
  })),

  http.post("/api/runs/:id/interrupt", authed(({ params }) => {
    const run = db().runs.find((row) => row.id === params.id)
    if (!run) return fail(404, "run not found")
    if (!isRunActive(run.id)) return fail(409, "the run has already finished")
    interrupt(run.id)
    return new HttpResponse(null, { status: 202 })
  })),
  http.post("/api/runs/:id/retry", authed(async ({ params, request }) => {
    const run = db().runs.find((row) => row.id === params.id)
    if (!run) return fail(404, "run not found")
    if (isRunActive(run.id)) return fail(409, "the run is still in progress")
    const thread = db().threads.find((row) => row.id === run.thread_id)
    const session = db().sessions.find((row) => row.id === thread?.session_id)
    if (!thread || !session?.model) return fail(400, "this session has no model")
    const input = db().transcripts[run.id]?.find((event) => event.kind === "input")
    const content =
      ((input?.payload as { content?: { type: string; text?: string }[] } | null)?.content ?? [])
        .map((block) => block.text ?? "")
        .join("\n") || "retry"
    const { mode } = await body<{ mode?: string }>(request)
    const next = startRun({ sessionId: session.id, threadId: thread.id, content, model: session.model })
    return HttpResponse.json({ run_id: next.id, thread_id: thread.id, mode: mode ?? "full" }, { status: 202 })
  })),
]

function exchangesFor(runId: Uuid): ExchangeDetail[] {
  const run = db().runs.find((row) => row.id === runId)
  if (!run) return []
  const events = db().transcripts[runId] ?? []
  return events
    .filter((event) => event.kind === "message")
    .map((event, index): ExchangeDetail => {
      const payload = event.payload as { usage?: Exchange["usage"] } | null
      const exchange: Exchange = {
        id: `${runId.slice(0, 24)}${String(index).padStart(12, "0")}`,
        run_id: runId,
        usage: payload?.usage ?? null,
        outcome: { type: "ok" },
        expected_cache_tokens: 8_000,
        actual_cache_tokens: 8_400,
        has_provider_events: true,
        canonical: true,
        started_at: event.created_at - 2,
        completed_at: event.created_at,
      }
      return {
        ...exchange,
        request: JSON.stringify(
          { model: "mock", messages: [{ role: "user", content: "(recorded by the Storybook mock)" }], stream: true },
          null,
          2,
        ),
        provider_events: [{ type: "message_start" }, { type: "message_stop" }],
        canonical_blob: event.payload,
      }
    })
}

export const handlers = [
  ...surgeHandlers,
  ...identityHandlers,
  ...credentialHandlers,
  ...modelHandlers,
  ...hostHandlers,
  ...sessionHandlers,
]
