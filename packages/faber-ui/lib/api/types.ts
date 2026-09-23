/**
 * Wire types for the faber REST API, mirroring the `*Response` / `*Request`
 * structs in `crates/api/src/routes/`. Field names stay snake_case: they are
 * protocol values, not identifiers.
 *
 * Two timestamp conventions cross this boundary, matching the two halves of
 * the schema — `users`, `credentials`, and `models` store `TIMESTAMPTZ` and
 * serialize as RFC 3339; the conversation tables store epoch seconds and
 * serialize as a number. See `crates/api/src/models/mod.rs`.
 */

export type Uuid = string

/** RFC 3339 instant, e.g. `2026-08-11T09:12:00.482Z`. */
export type Timestamp = string

/** Epoch **seconds** — not milliseconds. `new Date(value * 1000)` to widen. */
export type EpochSeconds = number

export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue }

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/**
 * Local user identity only. username/display_name/avatar_url are not stored here —
 * fetch those from Surge's own whoami rather than this API.
 */
export interface Me {
  id: Uuid
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/** The key itself is write-only — only its last four characters ever come back. */
export type CredentialKind = "api_key" | "ssh_key"

export interface Credential {
  id: Uuid
  label: string
  kind: CredentialKind
  last_four: string
  created_at: Timestamp
}

export interface CreateCredentialRequest {
  /** Unique per user; a duplicate is a 400, not a conflict. */
  label: string
  kind: CredentialKind
  key: string
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/** Provider protocol the model speaks. */
export type Wire = "openai" | "anthropic"

/**
 * How much of a replayed assistant turn's reasoning goes back to the provider.
 *
 * Lives under `capabilities.reasoning_history`; omitting it leaves the wire's
 * own default (Anthropic sends reasoning back whole, OpenAI drops it).
 * `"full"` sends the reasoning and its signature, `"text"` the reasoning
 * without the signature, `"omitted"` neither.
 */
export type ReasoningHistory = "full" | "text" | "omitted"

/**
 * How much a model should spend on a turn. The levels a given model actually
 * offers are declared on its own row — see {@link ThinkingCapability}.
 */
export type Effort = "low" | "medium" | "high" | "xhigh" | "max"

/**
 * What a session's thinking knob is set to.
 *
 * `"off"` sends "do not reason"; `"on"` reasons at whatever effort the
 * provider picks, which is all a model with no levels of its own can be told;
 * an {@link Effort} names a level. `null` — the knob left alone — is none of
 * these: it falls through to whatever the model's definition defaults to.
 */
export type ThinkingSelection = "off" | "on" | Effort

/**
 * A model's reasoning knob, under `capabilities.thinking`.
 *
 * Absent, or `supported: false`, means this model has no thinking knob at all
 * — the picker hides it and runs against it send no reasoning fields.
 */
export interface ThinkingCapability {
  supported: boolean
  /** The levels to offer, in display order. Empty is on/off and nothing more. */
  efforts: Effort[]
  /** What a session that never picked runs at. Must be one of `efforts`. */
  default_effort?: Effort | null
}

export interface ModelConfig {
  id: Uuid
  alias: string
  base_url: string
  wire: Wire
  /** The provider's own id for the model, e.g. `claude-opus-5`. */
  wire_id: string
  family: string | null
  credential_id: Uuid | null
  params: JsonValue
  capabilities: JsonValue
  created_at: Timestamp
}

export interface CreateModelRequest {
  alias: string
  base_url: string
  wire: Wire
  wire_id: string
  family?: string | null
  /** Must name a credential the caller owns, or the request is a 400. */
  credential_id?: Uuid | null
  params?: JsonValue
  capabilities?: JsonValue
}

/** Every field is optional; `null` on `family`/`credential_id` clears the column. */
export interface UpdateModelRequest {
  alias?: string
  base_url?: string
  wire?: Wire
  wire_id?: string
  family?: string | null
  credential_id?: Uuid | null
  params?: JsonValue
  capabilities?: JsonValue
}

// ---------------------------------------------------------------------------
// Model presets and providers
// ---------------------------------------------------------------------------

/**
 * What a published model can do, from the AI Model Directory.
 *
 * `vision` is derived from the input modalities (the source states it as
 * `image`, not as a feature flag); the rest mirror the source's `features`.
 */
export interface ModelPresetCapabilities {
  vision: boolean
  attachment: boolean
  reasoning: boolean
  tools: boolean
  structured_output: boolean
  /**
   * Whether the source says the endpoint takes a temperature at all. Absent
   * there for most models, which reads as `false` — "unstated", not "refuses".
   */
  temperature: boolean
}

/** US dollars per million tokens. `null` is unknown, not free. */
export interface ModelPresetPricing {
  input: number | null
  output: number | null
  cache_read: number | null
  cache_write: number | null
  input_audio: number | null
  output_audio: number | null
  reasoning: number | null
}

/** Token budgets, where the source states them. */
export interface ModelPresetLimits {
  context: number | null
  input: number | null
  output: number | null
}

export interface ModelPresetModalities {
  input: string[]
  output: string[]
}

/**
 * One published model offer.
 *
 * Not a {@link ModelConfig}: a preset carries no credential and nothing here
 * routes a request. A caller sees their own presets and the system's; only
 * `owned` ones are theirs to change.
 */
export interface ModelPreset {
  /** Row handle for CRUD. `preset_id`, not the model `id` below. */
  preset_id: string
  /** Whether this preset belongs to the caller, as opposed to the system. */
  owned: boolean
  /** RFC 3339 timestamp of when the row was written. */
  created_at: string
  /** The publisher's key, e.g. `anthropic`. */
  provider: string
  provider_name: string
  /** The model id as served, e.g. `claude-opus-5`. */
  id: string
  name: string
  capabilities: ModelPresetCapabilities
  pricing: ModelPresetPricing
  limits: ModelPresetLimits
  modalities: ModelPresetModalities
  /** Epoch seconds, when the source states one. */
  release_date: number | null
  last_updated: number | null
  knowledge_cutoff: number | null
  open_weights: boolean | null
}

/** A publisher a preset points at. */
export interface ModelPresetProvider {
  /** Row handle for CRUD. `provider_id`, not the key `id` below. */
  provider_id: string
  /** The publisher's key, e.g. `anthropic`. */
  id: string
  name: string
  website: string | null
  /** Informational: a preset never routes a request to this. */
  api_base_url: string | null
  model_count: number
  /** Whether this provider belongs to the caller, as opposed to the system. */
  owned: boolean
  /** RFC 3339 timestamp of when the row was written. */
  created_at: string
}

export interface CreateModelProviderRequest {
  /** The publisher's key, e.g. `anthropic`. */
  id: string
  name: string
  website?: string | null
  api_base_url?: string | null
}

export interface UpdateModelProviderRequest {
  name?: string
  website?: string | null
  api_base_url?: string | null
}

export interface CreateModelPresetRequest {
  /** The caller's own provider this preset is published by. */
  provider_id: string
  /** The model id as served, e.g. `claude-opus-5`. */
  id: string
  name: string
  capabilities?: ModelPresetCapabilities
  pricing?: ModelPresetPricing
  limits?: ModelPresetLimits
  modalities?: ModelPresetModalities
  release_date?: number | null
  last_updated?: number | null
  knowledge_cutoff?: number | null
  open_weights?: boolean | null
}

export interface UpdateModelPresetRequest {
  provider_id?: string
  id?: string
  name?: string
  capabilities?: ModelPresetCapabilities
  pricing?: ModelPresetPricing
  limits?: ModelPresetLimits
  modalities?: ModelPresetModalities
  release_date?: number | null
  last_updated?: number | null
  knowledge_cutoff?: number | null
  open_weights?: boolean | null
}

/** A page of preset models. `total` is the count after filtering. */
export interface ModelPresetPage {
  total: number
  limit: number
  offset: number
  items: ModelPreset[]
}

// Written as a type alias, not an interface: only an alias picks up the
// implicit index signature the client's query-string builder takes.

/**
 * Absent fields do not filter. `limit` is clamped server-side to 1..=500.
 * `owned` splits the two halves a caller can see: `true` for their own
 * presets, `false` for the system's.
 */
export type ListModelPresetsQuery = {
  provider?: string
  q?: string
  vision?: boolean
  reasoning?: boolean
  tools?: boolean
  owned?: boolean
  limit?: number
  offset?: number
}

// ---------------------------------------------------------------------------
// Execution environments
// ---------------------------------------------------------------------------

/** Public server configuration used to shape frontend host controls. */
export interface FaberConfig {
  allow_local_hosts: boolean
}

/**
 * How faber reaches the machine.
 *
 * `agent` inverts the direction: faber never dials such a host, a daemon
 * installed on it dials faber and holds the connection open. That is why an
 * agent host carries no address — there is nothing to connect to.
 */
export type Transport = "local" | "ssh" | "agent"

/**
 * What faber execs into once it has reached the machine. Deliberately not
 * derived from `docker_endpoint` — an SSH host that *could* run docker but is
 * deliberately used direct is a real configuration.
 */
export type ExecMode = "direct" | "docker"

/**
 * One past observation of a host. Advisory only, and never a status: it says
 * what happened at `probed_at`, not what is true now. Render it as
 * "last reachable 3h ago" / "last attempt: connection refused" — the
 * authoritative answer to "is it up" is the next connection attempt.
 */
export interface HostProbe {
  id: Uuid
  host_id: Uuid
  /** Set when the observation was scoped to one registered container. */
  container_id: Uuid | null
  probed_at: Timestamp
  ok: boolean
  /** Populated when `ok` is false. */
  error: string | null
  os: string | null
  arch: string | null
  shell: string | null
  /** Capability manifest, e.g. `{ "git": "2.43.0" }`. */
  tools: JsonValue | null
  root_path: string | null
}

/**
 * A registration pointing at a container on a docker-mode host. The row asserts
 * *faber knows about this container*, not *this container exists*.
 */
export interface HostContainer {
  id: Uuid
  host_id: Uuid
  /** Name or id, resolved lazily — it may no longer resolve to anything. */
  container_ref: string
  name: string | null
  /** Normalized agent-visible root; always absolute. */
  root_path: string
  created_at: Timestamp
  /** State of the *registration*, not of the container. */
  unregistered_at: Timestamp | null
  /** When true, new sessions auto-bind this container without an @mention. */
  bind_by_default: boolean
  /**
   * Whether faber created this container. The two are rendered differently on
   * purpose: unregistering a managed container can also destroy it, and
   * unregistering one faber merely knows about never can.
   */
  managed: boolean
  managed_at: Timestamp | null
  /** The template it came from, or `null` — including when the template was
   *  deleted since. Provenance; nothing resolves through it. */
  image_id: Uuid | null
}

/** A reachable machine. Everything else in this section hangs off one. */
export interface Host {
  id: Uuid
  name: string
  transport: Transport
  exec_mode: ExecMode
  /** `user@host:port`. Set if and only if `transport` is `ssh`. */
  ssh_address: string | null
  /** Secret-store handle, never key material. */
  ssh_key_ref: string | null
  /** `unix://` or `tcp://`; `null` means the host's local socket. */
  docker_endpoint: string | null
  /**
   * The agent-visible root for direct execution. `null` means this host cannot
   * be bound to a session on its own — only containers on it can.
   */
  root_path: string | null
  created_at: Timestamp
  /** Operator intent, not observed state — an unreachable host is still enabled. */
  disabled_at: Timestamp | null
  /** When true, new sessions auto-bind this host without an @mention. */
  bind_by_default: boolean
  /** Registrations that have not been unregistered, oldest first. */
  containers: HostContainer[]
  /** The most recent observation, or `null` if never probed. */
  last_probe: HostProbe | null
}

/**
 * A one-time bootstrap token and the command that carries it onto the
 * machine. Issued per request: asking again supersedes whatever was issued
 * before, so only the newest command still works.
 */
export interface AgentEnrollment {
  /** Shown once, inside {@link AgentEnrollment.install_command}. */
  token: string
  expires_at: Timestamp
  /** Copy-pasteable one-liner: it fetches the installer, which downloads the
   *  daemon and hands it the token. */
  install_command: string
}

/**
 * Where an agent host's daemon stands. Neither field is stored as state on
 * the host: `connected` is read from the live connection each time it is
 * asked for, and stops being true the moment the connection is gone.
 */
export interface AgentStatus {
  connected: boolean
  /** When the daemon exchanged its bootstrap token, or `null` if nobody has
   *  run the install command yet. */
  enrolled_at: Timestamp | null
}

export interface CreateHostRequest {
  name: string
  transport: Transport
  exec_mode: ExecMode
  /** Required when `transport` is `ssh`, rejected when it is `local`. */
  ssh_address?: string | null
  ssh_key_ref?: string | null
  docker_endpoint?: string | null
  /** Required for direct hosts; the agent-visible filesystem root. */
  root_path?: string | null
  /** When true, new sessions auto-bind this host without an @mention. */
  bind_by_default?: boolean
}

/** Every field is optional; `null` clears a nullable column. */
export interface UpdateHostRequest {
  name?: string
  transport?: Transport
  exec_mode?: ExecMode
  ssh_address?: string | null
  ssh_key_ref?: string | null
  docker_endpoint?: string | null
  /** `null` clears the direct host root. */
  root_path?: string | null
  /** `true` stamps `disabled_at`, `false` clears it. */
  disabled?: boolean
  /** When true, new sessions auto-bind this host without an @mention. */
  bind_by_default?: boolean
}

export interface CreateContainerRequest {
  container_ref: string
  name?: string | null
  /** Must be absolute — a relative root does not transfer between hosts. */
  root_path: string
  /** When true, new sessions auto-bind this container without an @mention. */
  bind_by_default?: boolean
}

/**
 * Starts a container from an image and registers the result in one call.
 *
 * Unlike {@link CreateContainerRequest}, which only records a container the
 * user already runs, this asks faber to create one — the "Create" half of the
 * Add menu on `/environments`.
 */
export interface SpawnContainerRequest {
  /** The template to start from. */
  image_id: Uuid
  /** User label, and the container's name on the daemon when set. */
  name?: string | null
  /** Defaults to the image's `default_root_path` when omitted. */
  root_path?: string
}

export interface UpdateContainerRequest {
  container_ref?: string
  name?: string | null
  root_path?: string
  /** `false` re-registers a row that was unregistered earlier. */
  unregistered?: boolean
  /** When true, new sessions auto-bind this container without an @mention. */
  bind_by_default?: boolean
}

/** Appended to the host's observation log. There is no route to amend one. */
export interface RecordProbeRequest {
  container_id?: Uuid | null
  ok: boolean
  /** Required when `ok` is false. */
  error?: string | null
  os?: string | null
  arch?: string | null
  shell?: string | null
  tools?: JsonValue | null
  root_path?: string | null
}

export type ListContainersQuery = {
  /** Unregistered rows are history, and hidden unless asked for. */
  include_unregistered?: boolean
}

export type ListProbesQuery = {
  limit?: number
}

/**
 * A spawn template. Not a host, not a container, and not owned by either —
 * nothing points at it, because a spawned container's origin is provenance
 * nobody branches on.
 */
export interface Image {
  id: Uuid
  name: string
  /** Registry ref, e.g. `ghcr.io/acme/dev:latest`. */
  reference: string
  default_mounts: JsonValue | null
  default_root_path: string
  created_at: Timestamp
}

export interface CreateImageRequest {
  name: string
  reference: string
  default_mounts?: JsonValue | null
  /** Must be absolute, same as a container's `root_path`. */
  default_root_path: string
}

export interface UpdateImageRequest {
  name?: string
  reference?: string
  default_mounts?: JsonValue | null
  default_root_path?: string
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

export interface Workspace {
  id: Uuid
  /** `user` for the personal workspace, `common` for a shared one. */
  kind: "user" | "common"
  /** Set only on a `user` workspace. */
  user_id: Uuid | null
  created_at: EpochSeconds
}

// ---------------------------------------------------------------------------
// Sessions and threads
// ---------------------------------------------------------------------------

export interface Session {
  id: Uuid
  workspace_id: Uuid
  title: string | null
  created_at: EpochSeconds
  /** Set while the session is closed; `PATCH { closed: false }` reopens it. */
  closed_at: EpochSeconds | null
  /**
   * Alias of the model the next message goes to, or `null` if nothing has
   * picked one. Persisted per session: the choice is part of what a thread
   * *is*, so opening a second one never re-aims the first.
   */
  model: string | null
  /** The thinking knob as the user left it; `null` is the model's own default. */
  thinking_effort: ThinkingSelection | null
}

/** A session is always created with its root thread — the API returns both. */
export interface CreatedSession extends Session {
  root_thread: Thread
  /** Environments auto-bound by their `bind_by_default` flag on session creation. */
  default_environments: string[]
}

export interface CreateSessionRequest {
  /** Defaults to the caller's personal workspace. */
  workspace_id?: Uuid
  title?: string
}

export interface UpdateSessionRequest {
  /** `null` clears the title; omit the key to leave it unchanged. */
  title?: string | null
  /** `true` stamps `closed_at`, `false` reopens. */
  closed?: boolean
  /**
   * The model alias new messages go to; `null` clears the selection. Where the
   * model picker writes, so a pick survives a reload with nothing sent.
   */
  model?: string | null
  /** `null` clears the knob back to the model's own default. */
  thinking_effort?: ThinkingSelection | null
}

export interface Thread {
  id: Uuid
  session_id: Uuid
  /** Set on a fork, always together with `forked_at_seq`. */
  parent_id: Uuid | null
  forked_at_seq: number | null
  /** Core-owned allocator — the next `spine.seq` this thread will hand out. */
  next_seq: number
  created_at: EpochSeconds
}

/** Both fields or neither: a fork needs its source, a root thread takes no arguments. */
export interface CreateThreadRequest {
  parent_id?: Uuid
  /** Inclusive position in the parent, in `0..parent.next_seq`. */
  forked_at_seq?: number
}

/** One position in a thread's canonical history chain. */
export interface SpineEntry {
  seq: number
  exchange_id: Uuid
  /** `true` when a best-of-N winner was committed deliberately. */
  explicit_commit: boolean
  created_at: EpochSeconds
}

export interface Run {
  id: Uuid
  thread_id: Uuid
  created_at: EpochSeconds
  completed_at: EpochSeconds | null
}

/**
 * One harness-yielded event — what the user saw, in order. Not the provider
 * exchange; the two are separate logs and neither derives the other.
 */
export interface TranscriptEvent {
  id: Uuid
  seq: number
  /** Free-form tag, deliberately not an enum on either side. */
  kind: string
  payload: JsonValue
  created_at: EpochSeconds
}

/**
 * One provider call Core observed at the capability boundary — the request
 * bytes as sent and the events as received. Ground truth, as opposed to the
 * transcript's record of what the user saw (`history-abstract.md` H2/H7).
 */
export interface Exchange {
  id: Uuid
  run_id: Uuid
  /** Provider-reported token accounting, or `null` when it reported none. */
  usage: JsonValue | null
  /** How the call ended, e.g. `{ "type": "ok" }`. */
  outcome: JsonValue | null
  expected_cache_tokens: number
  actual_cache_tokens: number | null
  /** Whether the provider event stream was recorded. */
  has_provider_events: boolean
  /**
   * True on the exchange a `spine` row names — the committed lineage. The rest
   * are the garbage class (best-of-N losers, repair attempts).
   */
  canonical: boolean
  started_at: EpochSeconds
  completed_at: EpochSeconds | null
}

/**
 * One exchange with the bytes behind its digests. Fetched per exchange because
 * a request blob carries the whole context a call sent.
 */
export interface ExchangeDetail extends Exchange {
  /** Request bytes as sent, decoded as UTF-8. Usually a JSON document. */
  request: string
  /** Provider events as received, when they were recorded and parse as JSON. */
  provider_events: JsonValue | null
  /** The canonical lineage this exchange committed, when it committed one. */
  canonical_blob: JsonValue | null
}

// ---------------------------------------------------------------------------
// Retry
// ---------------------------------------------------------------------------

export type RetryMode = "full" | "from_checkpoint"

export interface RetryRequest {
  mode?: RetryMode
}

export interface RetryResponse {
  run_id: Uuid
  thread_id: Uuid
  mode: string
}

// ---------------------------------------------------------------------------
// Messages and streaming
// ---------------------------------------------------------------------------

export interface SendMessageRequest {
  content: string
  /**
   * A model **alias** the caller owns (what you'd type as `faber -m fast`),
   * not a provider model id.
   *
   * Optional, and remembered: naming one here both sends this message to it
   * and leaves the session pointed at it. Omit it to run on whatever the
   * session already holds — a session holding nothing is a 400, not a guess.
   */
  model?: string
  /** The thinking knob, remembered the same way. */
  thinking_effort?: ThinkingSelection
  /** Required once a session has more than one thread. */
  thread_id?: Uuid
}

/** `202 Accepted` — the run is detached and observed through the stream. */
export interface SendMessageResponse {
  run_id: Uuid
  thread_id: Uuid
  /**
   * Environments this message added to the session, in the order they were
   * tagged. Empty when it tagged none, or only ones the session already had.
   */
  added_environments: string[]
}

/**
 * One event off `streamSession`. `kind` is the harness event's own `type` for
 * model output, plus three the API adds: `input` (the user's own turn) and the
 * terminal `run_end` / `run_error`, and session metadata such as `session_title`.
 *
 * The terminal markers are live-only — they are stream control, not something
 * the harness yielded, so they are never persisted. A client that connects
 * after a run finished learns that from `Run.completed_at`.
 */
export interface StreamEvent {
  run_id: Uuid
  /** Position within `run_id`, **not** within the session. `-1` on a marker. */
  seq: number
  kind: string
  payload: JsonValue
}

/**
 * Resume cursor. Both fields together or neither — `seq` is unique per run,
 * not per session, so one alone names nothing and is rejected as a 400.
 */
export type StreamQuery = {
  run_id?: Uuid
  after_seq?: number
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

// Written as type aliases, not interfaces: only an alias picks up the implicit
// index signature the client's query-string builder takes.

/** `limit` is clamped server-side to 1..=500 and defaults to 100. */
export type ListSessionsQuery = {
  workspace_id?: Uuid
  limit?: number
}

export type TranscriptQuery = {
  /** Only events strictly after this `seq` — poll the tail without refetching. */
  after_seq?: number
  limit?: number
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

/**
 * One name a session could be told to reach, and what is behind it.
 *
 * This is what the `@` picker reads, so a name it offers is a name that
 * resolves. Labels are short where they can be and qualified as `host/name`
 * where two would otherwise collide.
 */
export interface EnvironmentCandidate {
  /** What the user types after `@`. */
  label: string
  kind: "host" | "container"
  host_id: Uuid
  host_name: string
  container_id: Uuid | null
  root_path: string
  /** Operator intent on the host. Shown rather than hidden — a name missing
   *  from the picker looks like a name that does not exist. */
  disabled: boolean
  /** When true, new sessions auto-bind this environment without an @mention. */
  bind_by_default: boolean
}

/** One binding a session has, or had. */
export interface SessionEnvironment {
  label: string
  host_id: Uuid
  container_id: Uuid | null
  added_at: EpochSeconds
  /**
   * Set once unbound. The row stays and so does the claim on the name: a label
   * that meant two machines would make the earlier half of the transcript
   * wrong.
   */
  removed_at: EpochSeconds | null
}


