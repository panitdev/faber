/**
 * In-memory stand-in for the faber API's database, so Storybook can exercise
 * every screen without a running `crates/api` or Surge.
 *
 * One module-level instance, reset before every story (see
 * `.storybook/preview.tsx`) — a story that deletes a host must not leave the
 * next one without it. Everything the handlers in `./handlers.ts` answer with
 * is read from here, and every mutation writes back here, so create → list →
 * edit → delete round-trips within a story the way it would against the API.
 */

import type {
  AgentStatus,
  Credential,
  FaberConfig,
  HostProbe,
  Image,
  Me,
  ModelConfig,
  CreatorModel,
  ModelPreset,
  ModelPresetProvider,
  Run,
  Session,
  SessionEnvironment,
  Thread,
  TranscriptEvent,
  Uuid,
  Workspace,
} from "@/lib/api"
import type { Host, HostContainer } from "@/lib/api"

import { buildDefaultSeed } from "./fixtures"

/** Surge's `GET /v1/whoami` body — kept loose, the mock only ever echoes it. */
export type SurgeIdentitySession = {
  id: string
  identity: {
    id: string
    username: string
    display_name: string
    avatar_url: string | null
    state: "active" | "disabled"
    created_at: string
    updated_at: string
  }
  issued_at: string
  expires_at: string
  authenticated_via: "password"
  policy?: {
    required: { totp: boolean; passphrase: boolean }
    has: { totp: boolean; passphrase: boolean }
    compliant: boolean
  }
}

/** A host row without the parts the API composes onto it at read time. */
export type HostRow = Omit<Host, "containers" | "last_probe">

export type MockDb = {
  config: FaberConfig
  me: Me
  /** `null` is signed out: whoami answers 401 and the app shows sign-in. */
  surgeSession: SurgeIdentitySession | null
  credentials: Credential[]
  models: ModelConfig[]
  providers: ModelPresetProvider[]
  /** Read-only, like the API's: the catalog's creator models. */
  creatorModels: CreatorModel[]
  /** Stored with their overrides and link; resolved fields are kept in step by the handlers. */
  presets: ModelPreset[]
  hosts: HostRow[]
  containers: HostContainer[]
  probes: HostProbe[]
  agents: Record<Uuid, AgentStatus>
  images: Image[]
  workspaces: Workspace[]
  sessions: Session[]
  threads: Thread[]
  runs: Run[]
  /** Durable transcript per run, in `seq` order — what `listTranscript` pages. */
  transcripts: Record<Uuid, TranscriptEvent[]>
  sessionEnvironments: Record<Uuid, SessionEnvironment[]>
}

/**
 * Knobs a story can set through `parameters.mockApi`.
 *
 * - `scenario` picks the starting data: the populated default, a brand-new
 *   account with nothing configured, or signed out.
 * - `seed` then edits that data in place, for the one-off states a single
 *   story wants (a disabled host, a session with a failed run, …).
 * - `latency` is the delay, in ms, before every REST answer. Loading states
 *   are part of what a story is for, so it defaults to something visible.
 * - `replySpeed` scales the simulated agent's streaming: 1 is the default
 *   pace, 0 emits everything as fast as the event loop allows.
 */
export type MockApiParameters = {
  scenario?: "default" | "empty" | "signedOut"
  seed?: (db: MockDb) => void
  latency?: number
  replySpeed?: number
}

export const DEFAULT_LATENCY_MS = 120

let current: MockDb = buildDefaultSeed()
let settings: Required<Pick<MockApiParameters, "latency" | "replySpeed">> = {
  latency: DEFAULT_LATENCY_MS,
  replySpeed: 1,
}

/** The live database. Read it fresh on every request — it is replaced on reset. */
export function db(): MockDb {
  return current
}

export function mockSettings() {
  return settings
}

export function resetDb(parameters: MockApiParameters = {}): MockDb {
  const scenario = parameters.scenario ?? "default"
  const next = buildDefaultSeed()

  if (scenario === "empty") {
    next.credentials = []
    next.models = []
    next.hosts = []
    next.containers = []
    next.probes = []
    next.agents = {}
    next.images = []
    next.sessions = []
    next.threads = []
    next.runs = []
    next.transcripts = {}
    next.sessionEnvironments = {}
    next.presets = next.presets.filter((preset) => !preset.owned)
    next.providers = next.providers.filter((provider) => !provider.owned)
  }

  if (scenario === "signedOut") next.surgeSession = null

  parameters.seed?.(next)

  current = next
  settings = {
    latency: parameters.latency ?? DEFAULT_LATENCY_MS,
    replySpeed: parameters.replySpeed ?? 1,
  }
  return next
}

// ---------------------------------------------------------------------------
// Small helpers the handlers and fixtures share
// ---------------------------------------------------------------------------

export function uuid(): Uuid {
  return crypto.randomUUID()
}

export function nowIso(): string {
  return new Date().toISOString()
}

export function nowEpoch(): number {
  return Math.floor(Date.now() / 1000)
}

/** A host as the API returns it: active containers and newest probe attached. */
export function composeHost(row: HostRow): Host {
  const { containers, probes } = current
  const newestProbe = probes
    .filter((probe) => probe.host_id === row.id)
    .sort((a, b) => b.probed_at.localeCompare(a.probed_at))[0]
  return {
    ...row,
    containers: containers
      .filter((container) => container.host_id === row.id && container.unregistered_at === null)
      .sort((a, b) => a.created_at.localeCompare(b.created_at)),
    last_probe: newestProbe ?? null,
  }
}
