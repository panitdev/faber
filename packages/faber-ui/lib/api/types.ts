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
}

export interface CreateContainerRequest {
  container_ref: string
  name?: string | null
  /** Must be absolute — a relative root does not transfer between hosts. */
  root_path: string
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


