/**
 * Starting data for the mock API. Shapes follow `lib/api/types.ts` exactly;
 * transcripts follow what `crates/api` persists (whole `message` events, not
 * the deltas that streamed them — see `lib/thread/transcript.ts`).
 *
 * Timestamps are relative to "now" so relative labels ("3h ago") read sensibly
 * whenever Storybook is opened.
 */

import type {
  ContentBlock,
  TranscriptMessage,
} from "@/lib/thread/transcript"
import type {
  HostContainer,
  HostProbe,
  JsonValue,
  CreatorModel,
  ModelConfig,
  ModelCost,
  ModelOverrides,
  ModelPreset,
  ModelPresetProvider,
  ModelPresetSpec,
  ModelSpec,
  Run,
  Session,
  Thread,
  TranscriptEvent,
  Uuid,
} from "@/lib/api"

import type { HostRow, MockDb } from "./db"

// Fixed ids, so a story can deep-link (`/session/$sessionId`) or seed against
// a known row without searching for it.
export const IDS = {
  user: "00000000-0000-4000-8000-000000000001",
  workspace: "00000000-0000-4000-8000-000000000002",
  credentialAnthropic: "00000000-0000-4000-8000-00000000c001",
  credentialOpenRouter: "00000000-0000-4000-8000-00000000c002",
  credentialSsh: "00000000-0000-4000-8000-00000000c003",
  modelOpus: "00000000-0000-4000-8000-00000000a001",
  modelFast: "00000000-0000-4000-8000-00000000a002",
  providerAnthropic: "00000000-0000-4000-8000-00000000b001",
  providerOpenAI: "00000000-0000-4000-8000-00000000b002",
  providerMine: "00000000-0000-4000-8000-00000000b003",
  providerOpenRouter: "00000000-0000-4000-8000-00000000b004",
  creatorOpus: "00000000-0000-4000-8000-00000000c101",
  creatorSonnet: "00000000-0000-4000-8000-00000000c102",
  creatorHaiku: "00000000-0000-4000-8000-00000000c103",
  creatorGpt: "00000000-0000-4000-8000-00000000c104",
  presetOpus: "00000000-0000-4000-8000-00000000d001",
  presetSonnet: "00000000-0000-4000-8000-00000000d002",
  presetHaiku: "00000000-0000-4000-8000-00000000d003",
  presetGpt: "00000000-0000-4000-8000-00000000d004",
  presetMine: "00000000-0000-4000-8000-00000000d005",
  presetRouterOpus: "00000000-0000-4000-8000-00000000d006",
  hostLaptop: "00000000-0000-4000-8000-00000000e001",
  hostBuildbox: "00000000-0000-4000-8000-00000000e002",
  hostAgent: "00000000-0000-4000-8000-00000000e003",
  containerDev: "00000000-0000-4000-8000-00000000f001",
  containerScratch: "00000000-0000-4000-8000-00000000f002",
  imageDev: "00000000-0000-4000-8000-000000009001",
  sessionRefactor: "00000000-0000-4000-8000-000000001001",
  sessionFlaky: "00000000-0000-4000-8000-000000001002",
  sessionIdeas: "00000000-0000-4000-8000-000000001003",
  sessionEmpty: "00000000-0000-4000-8000-000000001004",
} as const

const HOUR = 3600
const DAY = 24 * HOUR

function epochAgo(seconds: number): number {
  return Math.floor(Date.now() / 1000) - seconds
}

function isoAgo(seconds: number): string {
  return new Date(Date.now() - seconds * 1000).toISOString()
}

// ---------------------------------------------------------------------------
// Model catalog
// ---------------------------------------------------------------------------

const DATE_FORMAT_DAY = 10

/** A `YYYY-MM-DD` date `seconds` before now, the way the catalog writes dates. */
function dateAgo(seconds: number): string {
  return isoAgo(seconds).slice(0, DATE_FORMAT_DAY)
}

function creatorModel(
  creatorModelId: Uuid,
  id: string,
  name: string,
  overrides: Partial<ModelSpec> = {},
): CreatorModel {
  return {
    creator_model_id: creatorModelId,
    id,
    creator: id.split("/")[0],
    name,
    description: null,
    family: null,
    attachment: true,
    reasoning: true,
    tool_call: true,
    structured_output: true,
    temperature: true,
    knowledge: dateAgo(300 * DAY).slice(0, 7),
    release_date: dateAgo(120 * DAY),
    last_updated: dateAgo(30 * DAY),
    open_weights: false,
    limit: { context: 200_000, input: null, output: 64_000 },
    modalities: { input: ["text", "image"], output: ["text"] },
    license: null,
    preset_count: 0,
    ...overrides,
  }
}

const opusModel = creatorModel(IDS.creatorOpus, "anthropic/claude-opus-5", "Claude Opus 5", {
  family: "claude-opus",
  description: "Frontier model for long-horizon agentic work",
})
const sonnetModel = creatorModel(IDS.creatorSonnet, "anthropic/claude-sonnet-5", "Claude Sonnet 5", {
  family: "claude-sonnet",
  limit: { context: 1_000_000, input: null, output: 64_000 },
})
const haikuModel = creatorModel(IDS.creatorHaiku, "anthropic/claude-haiku-4-5", "Claude Haiku 4.5", {
  family: "claude-haiku",
})
const gptModel = creatorModel(IDS.creatorGpt, "openai/gpt-5", "GPT-5", {
  family: "gpt",
  temperature: false,
  limit: { context: 400_000, input: 272_000, output: 128_000 },
})

export const CREATOR_MODELS: CreatorModel[] = [opusModel, sonnetModel, haikuModel, gptModel]

const NO_OVERRIDES: ModelOverrides = {
  name: null,
  description: null,
  family: null,
  attachment: null,
  reasoning: null,
  tool_call: null,
  structured_output: null,
  temperature: null,
  knowledge: null,
  release_date: null,
  last_updated: null,
  open_weights: null,
  limit: null,
  modalities: null,
}

function cost(input: number, output: number, cacheRead: number | null, cacheWrite: number | null): ModelCost {
  return {
    input,
    output,
    cache_read: cacheRead,
    cache_write: cacheWrite,
    input_audio: null,
    output_audio: null,
    reasoning: null,
  }
}

/**
 * A preset row the way the mock stores it: the link and the overrides are the
 * truth, and the resolved fields are filled in by the handlers' `resolveRow`
 * — the same way the API lays a preset's overrides over its creator model.
 */
function preset(
  presetId: Uuid,
  provider: ModelPresetProvider,
  id: string,
  base: CreatorModel | null,
  fields: Partial<Pick<ModelPreset, "cost" | "status" | "reasoning_options">> & {
    overrides?: Partial<ModelOverrides>
  },
): ModelPreset {
  // Resolved here as the handlers' `resolveRow` would: the overrides over the
  // base, or over a bare description when there is none.
  const stated = Object.fromEntries(
    Object.entries(fields.overrides ?? {}).filter(([, value]) => value !== null),
  )
  return {
    ...(base ?? creatorModel("", id, id)),
    ...stated,
    preset_id: presetId,
    owned: provider.owned,
    created_at: isoAgo(60 * DAY),
    model_provider_id: provider.provider_id,
    creator_model_id: base?.creator_model_id ?? null,
    overrides: { ...NO_OVERRIDES, ...fields.overrides },
    provider: provider.id,
    provider_name: provider.name,
    id,
    base_model: base?.id ?? null,
    cost: fields.cost ?? null,
    reasoning_options: fields.reasoning_options ?? null,
    interleaved: null,
    status: fields.status ?? null,
  }
}

function provider(
  providerId: Uuid,
  id: string,
  name: string,
  owned = false,
): ModelPresetProvider {
  return {
    provider_id: providerId,
    id,
    name,
    doc: owned ? null : `https://${id}.com/docs/models`,
    api: owned ? "http://10.0.0.12:8000/v1" : null,
    npm: owned ? null : `@ai-sdk/${id}`,
    env: owned ? [] : [`${id.toUpperCase()}_API_KEY`],
    model_count: 0,
    owned,
    created_at: isoAgo(60 * DAY),
  }
}

const anthropic = provider(IDS.providerAnthropic, "anthropic", "Anthropic")
const openai = provider(IDS.providerOpenAI, "openai", "OpenAI")
const openrouter = provider(IDS.providerOpenRouter, "openrouter", "OpenRouter")
const homelab = provider(IDS.providerMine, "homelab", "Homelab", true)

export const PROVIDERS: ModelPresetProvider[] = [anthropic, openai, openrouter, homelab]

const opusPreset = preset(IDS.presetOpus, anthropic, "claude-opus-5", opusModel, {
  cost: cost(15, 75, 1.5, 18.75),
})
const haikuPreset = preset(IDS.presetHaiku, anthropic, "claude-haiku-4-5", haikuModel, {
  cost: cost(1, 5, 0.1, 1.25),
  reasoning_options: [{ type: "effort", values: ["none", "minimal", "low", "high"] }],
})

export const PRESETS: ModelPreset[] = [
  opusPreset,
  preset(IDS.presetSonnet, anthropic, "claude-sonnet-5", sonnetModel, {
    cost: cost(3, 15, 0.3, 3.75),
  }),
  haikuPreset,
  preset(IDS.presetGpt, openai, "gpt-5", gptModel, { cost: cost(1.25, 10, 0.125, null) }),
  // A router serving a creator's model under its own id, with a smaller
  // window: the only thing it stores besides its price is that difference.
  preset(IDS.presetRouterOpus, openrouter, "anthropic/claude-opus-5", opusModel, {
    cost: cost(16.5, 82.5, null, null),
    overrides: { limit: { context: 200_000, input: null, output: 32_000 } },
  }),
  // No creator model describes a local build, so it states everything itself.
  preset(IDS.presetMine, homelab, "qwen3-coder-32b", null, {
    overrides: {
      name: "Qwen3 Coder 32B (local)",
      attachment: false,
      reasoning: false,
      tool_call: true,
      structured_output: false,
      temperature: true,
      open_weights: true,
      limit: { context: 131_072, input: null, output: 16_384 },
      modalities: { input: ["text"], output: ["text"] },
    },
  }),
]

/** A preset as a model's `preset` field carries it: no CRUD handle. */
function spec(row: ModelPreset): ModelPresetSpec {
  const { preset_id, owned, created_at, model_provider_id, creator_model_id, overrides, ...rest } = row
  void [preset_id, owned, created_at, model_provider_id, creator_model_id, overrides]
  return rest
}

const opusSpec = spec(opusPreset)
const haikuSpec = spec(haikuPreset)

// ---------------------------------------------------------------------------
// Transcript builders — exported so stories can seed their own threads
// ---------------------------------------------------------------------------

export type ScriptedToolCall = {
  name: string
  input: JsonValue
  result: string
  isError?: boolean
}

export type ScriptedAssistantMessage = {
  thinking?: string
  text?: string
  tools?: ScriptedToolCall[]
}

export type CompletedRunSpec = {
  user: string
  messages: ScriptedAssistantMessage[]
  /** How the run ended. `null` leaves it in flight (no terminal marker). */
  end?: "run_end" | "run_error" | "run_interrupted" | null
  errorMessage?: string
  model?: string
  /** Environment notices the session added this turn, e.g. `Added @laptop`. */
  notices?: string[]
}

function usage(inputTokens: number, outputTokens: number, cost: number | null = null): JsonValue {
  return {
    inputTokens,
    outputTokens,
    cacheReadTokens: Math.round(inputTokens * 0.8),
    cacheWriteTokens: Math.round(inputTokens * 0.05),
    reasoningTokens: Math.round(outputTokens * 0.3),
    ...(cost === null ? {} : { cost }),
  }
}

function assistantContent(message: ScriptedAssistantMessage, toolIds: string[]): ContentBlock[] {
  const content: ContentBlock[] = []
  if (message.thinking) content.push({ type: "thinking", thinking: message.thinking, signature: "sig" })
  if (message.text) content.push({ type: "text", text: message.text })
  message.tools?.forEach((tool, index) => {
    content.push({ type: "tool_use", id: toolIds[index], name: tool.name, input: tool.input })
  })
  return content
}

/**
 * The durable events for one run, the way the API stores them: the user's
 * `input`, whole assistant `message`s (with usage), a `tool_result` per call,
 * and a terminal marker.
 */
export function buildRunEvents(runId: Uuid, spec: CompletedRunSpec, startedAt: number): TranscriptEvent[] {
  const events: TranscriptEvent[] = []
  let seq = 0
  const push = (kind: string, payload: JsonValue) => {
    events.push({ id: crypto.randomUUID(), seq: seq++, kind, payload, created_at: startedAt + seq })
  }

  const input: TranscriptMessage = { role: "user", content: [{ type: "text", text: spec.user }] }
  push("input", input as unknown as JsonValue)

  for (const notice of spec.notices ?? []) {
    push("environments", {
      role: "system",
      content: [{ type: "text", text: `<environments>${notice}</environments>` }],
    })
  }

  spec.messages.forEach((message, index) => {
    const toolIds = (message.tools ?? []).map((_, toolIndex) => `toolu_${runId.slice(-6)}_${index}_${toolIndex}`)
    const payload: TranscriptMessage = {
      role: "assistant",
      content: assistantContent(message, toolIds),
      usage: usage(12_000 + index * 1_800, 400 + (message.text?.length ?? 0), 0.021) as unknown as TranscriptMessage["usage"],
      model: spec.model ?? "opus",
    }
    push("message", payload as unknown as JsonValue)
    message.tools?.forEach((tool, toolIndex) => {
      push("tool_result", { toolUseId: toolIds[toolIndex], content: tool.result, isError: tool.isError ?? false })
    })
  })

  const end = spec.end === undefined ? "run_end" : spec.end
  if (end === "run_error") push("run_error", { message: spec.errorMessage ?? "the provider returned an error" })
  else if (end) push(end, null)

  return events
}

/**
 * Adds a session with one root thread and the given runs to `db`. Returns the
 * session id. Exported for stories that want a thread in a particular state.
 */
export function addSessionWithRuns(
  db: MockDb,
  options: {
    id?: Uuid
    title: string | null
    model?: string | null
    ageSeconds?: number
    closed?: boolean
    runs: CompletedRunSpec[]
  },
): Uuid {
  const sessionId = options.id ?? crypto.randomUUID()
  const threadId = crypto.randomUUID()
  const createdAt = epochAgo(options.ageSeconds ?? HOUR)

  const session: Session = {
    id: sessionId,
    workspace_id: IDS.workspace,
    title: options.title,
    created_at: createdAt,
    closed_at: options.closed ? createdAt + 60 : null,
    model: options.model === undefined ? "opus" : options.model,
    thinking_effort: null,
  }
  const thread: Thread = {
    id: threadId,
    session_id: sessionId,
    parent_id: null,
    forked_at_seq: null,
    next_seq: options.runs.length,
    created_at: createdAt,
  }

  db.sessions.unshift(session)
  db.threads.push(thread)

  options.runs.forEach((spec, index) => {
    const runId = crypto.randomUUID()
    const startedAt = createdAt + index * 120
    const run: Run = {
      id: runId,
      thread_id: threadId,
      created_at: startedAt,
      completed_at: spec.end === null ? null : startedAt + 30,
    }
    db.runs.push(run)
    db.transcripts[runId] = buildRunEvents(runId, spec, startedAt)
  })

  return sessionId
}

// ---------------------------------------------------------------------------
// The default, populated account
// ---------------------------------------------------------------------------

export function buildDefaultSeed(): MockDb {
  const userCreated = isoAgo(90 * DAY)

  const models: ModelConfig[] = [
    {
      id: IDS.modelOpus,
      alias: "opus",
      base_url: "https://api.anthropic.com",
      wire: "anthropic",
      wire_id: "claude-opus-5",
      family: "claude",
      credential_id: IDS.credentialAnthropic,
      params: { thinking: { supported: true, efforts: ["low", "medium", "high", "max"], default_effort: "high" } },
      preset_id: IDS.presetOpus,
      preset: opusSpec,
      created_at: isoAgo(40 * DAY),
    },
    {
      id: IDS.modelFast,
      alias: "fast",
      base_url: "https://openrouter.ai/api/v1",
      wire: "openai",
      wire_id: "anthropic/claude-haiku-4.5",
      family: "claude",
      credential_id: IDS.credentialOpenRouter,
      params: {},
      preset_id: IDS.presetHaiku,
      preset: haikuSpec,
      created_at: isoAgo(20 * DAY),
    },
  ]

  const hosts: HostRow[] = [
    {
      id: IDS.hostLaptop,
      name: "laptop",
      transport: "local",
      exec_mode: "direct",
      ssh_address: null,
      ssh_key_ref: null,
      docker_endpoint: null,
      root_path: "/home/geon/src",
      created_at: isoAgo(30 * DAY),
      disabled_at: null,
      bind_by_default: true,
    },
    {
      id: IDS.hostBuildbox,
      name: "buildbox",
      transport: "ssh",
      exec_mode: "docker",
      ssh_address: "geon@buildbox.lan:22",
      ssh_key_ref: IDS.credentialSsh,
      docker_endpoint: null,
      root_path: null,
      created_at: isoAgo(25 * DAY),
      disabled_at: null,
      bind_by_default: false,
    },
    {
      id: IDS.hostAgent,
      name: "gpu-node",
      transport: "agent",
      exec_mode: "direct",
      ssh_address: null,
      ssh_key_ref: null,
      docker_endpoint: null,
      root_path: "/srv/work",
      created_at: isoAgo(3 * DAY),
      disabled_at: null,
      bind_by_default: false,
    },
  ]

  const containers: HostContainer[] = [
    {
      id: IDS.containerDev,
      host_id: IDS.hostBuildbox,
      container_ref: "faber-dev",
      name: "dev",
      root_path: "/workspace",
      created_at: isoAgo(20 * DAY),
      unregistered_at: null,
      bind_by_default: false,
      managed: true,
      managed_at: isoAgo(20 * DAY),
      image_id: IDS.imageDev,
    },
    {
      id: IDS.containerScratch,
      host_id: IDS.hostBuildbox,
      container_ref: "3f9c2a1b7d04",
      name: null,
      root_path: "/root",
      created_at: isoAgo(10 * DAY),
      unregistered_at: null,
      bind_by_default: false,
      managed: false,
      managed_at: null,
      image_id: null,
    },
  ]

  const probes: HostProbe[] = [
    {
      id: crypto.randomUUID(),
      host_id: IDS.hostLaptop,
      container_id: null,
      probed_at: isoAgo(12 * 60),
      ok: true,
      error: null,
      os: "linux",
      arch: "x86_64",
      shell: "/bin/zsh",
      tools: { git: "2.47.1", node: "22.22.2", cargo: "1.91.0" },
      root_path: "/home/geon/src",
    },
    {
      id: crypto.randomUUID(),
      host_id: IDS.hostBuildbox,
      container_id: null,
      probed_at: isoAgo(3 * HOUR),
      ok: false,
      error: "ssh: connect to host buildbox.lan port 22: Connection refused",
      os: null,
      arch: null,
      shell: null,
      tools: null,
      root_path: null,
    },
  ]

  const db: MockDb = {
    config: { allow_local_hosts: true },
    me: { id: IDS.user },
    surgeSession: {
      id: "aeg_s_storybook",
      identity: {
        id: "00000000-0000-4000-8000-0000000000aa",
        username: "geon",
        display_name: "Geon",
        avatar_url: null,
        state: "active",
        created_at: userCreated,
        updated_at: userCreated,
      },
      issued_at: isoAgo(HOUR),
      expires_at: isoAgo(-30 * DAY),
      authenticated_via: "password",
      policy: {
        required: { totp: false, passphrase: false },
        has: { totp: false, passphrase: false },
        compliant: true,
      },
    },
    credentials: [
      { id: IDS.credentialAnthropic, label: "anthropic", kind: "api_key", last_four: "x9Qa", created_at: isoAgo(40 * DAY) },
      { id: IDS.credentialOpenRouter, label: "openrouter", kind: "api_key", last_four: "7f2e", created_at: isoAgo(20 * DAY) },
      { id: IDS.credentialSsh, label: "buildbox deploy key", kind: "ssh_key", last_four: "AAAB", created_at: isoAgo(25 * DAY) },
    ],
    models,
    // Cloned: handlers mutate rows in place, and every reset must start clean.
    providers: structuredClone(PROVIDERS).map((row) => ({
      ...row,
      model_count: PRESETS.filter((preset) => preset.model_provider_id === row.provider_id).length,
    })),
    creatorModels: structuredClone(CREATOR_MODELS),
    presets: structuredClone(PRESETS),
    hosts,
    containers,
    probes,
    agents: {
      [IDS.hostAgent]: { connected: true, enrolled_at: isoAgo(3 * DAY - 600) },
    },
    images: [
      {
        id: IDS.imageDev,
        name: "dev",
        reference: "ghcr.io/panitdev/faber-dev:latest",
        default_mounts: null,
        default_root_path: "/workspace",
        created_at: isoAgo(21 * DAY),
      },
    ],
    workspaces: [{ id: IDS.workspace, kind: "user", user_id: IDS.user, created_at: epochAgo(90 * DAY) }],
    sessions: [],
    threads: [],
    runs: [],
    transcripts: {},
    sessionEnvironments: {},
  }

  // Oldest first — `addSessionWithRuns` unshifts, so the newest ends up on top.
  addSessionWithRuns(db, {
    id: IDS.sessionIdeas,
    title: "Naming ideas for the scratch workspace",
    ageSeconds: 4 * DAY,
    model: "fast",
    closed: true,
    runs: [
      {
        user: "Give me five names for a per-session scratch workspace.",
        model: "fast",
        messages: [
          {
            text: "1. **Bench** — where work in progress sits\n2. **Tray**\n3. **Loft**\n4. **Scratchpad**\n5. **Desk**\n\n*Bench* reads best next to `@laptop` in a mention.",
          },
        ],
      },
    ],
  })

  addSessionWithRuns(db, {
    id: IDS.sessionFlaky,
    title: "Why is the probe test flaky?",
    ageSeconds: DAY,
    runs: [
      {
        user: "The probe test fails about one run in ten. Can you look?",
        messages: [
          {
            thinking: "Intermittent failure — likely a timing assumption. Run it in a loop to reproduce first.",
            text: "Let me reproduce it before guessing.",
            tools: [
              {
                name: "exec",
                input: { command: "for i in $(seq 20); do cargo test -p api probe -- --quiet || echo FAIL $i; done" },
                result: "FAIL 7\nFAIL 16\n",
              },
            ],
          },
        ],
        end: "run_error",
        errorMessage: "provider returned 529: overloaded",
      },
    ],
  })

  addSessionWithRuns(db, {
    id: IDS.sessionRefactor,
    title: "Split host dialogs into their own module",
    ageSeconds: 2 * HOUR,
    runs: [
      {
        user: "@laptop move the host dialogs out of `routes/environments.tsx` into `components/hosts/`.",
        notices: ["Added @laptop — host laptop, rooted at /home/geon/src."],
        messages: [
          {
            thinking:
              "Need to see how big the route file is and what the dialogs import before moving anything.",
            text: "I'll look at the current layout first.",
            tools: [
              {
                name: "exec",
                input: { command: "wc -l packages/faber-ui/src/routes/environments.tsx" },
                result: "912 packages/faber-ui/src/routes/environments.tsx",
              },
              {
                name: "read",
                input: { path: "packages/faber-ui/src/routes/environments.tsx" },
                result: "import { createFileRoute } from \"@tanstack/react-router\"\n…",
              },
            ],
          },
          {
            text: "The dialogs only depend on `lib/hosts`, so they move cleanly.",
            tools: [
              {
                name: "write",
                input: { path: "packages/faber-ui/components/hosts/host-dialogs.tsx" },
                result: "wrote 11.4 KB",
              },
              {
                name: "exec",
                input: { command: "bun x tsc --noEmit" },
                result: "",
              },
            ],
          },
          {
            text:
              "Done. The route file is down to ~340 lines.\n\n| File | Before | After |\n|---|---:|---:|\n| `routes/environments.tsx` | 912 | 341 |\n| `components/hosts/host-dialogs.tsx` | — | 588 |\n\nTypecheck passes. Want me to split `agent-install` out as well?",
          },
        ],
      },
      {
        user: "Yes, and keep the install command copy button.",
        messages: [
          {
            thinking: "The copy button lives inside the enroll dialog; it goes with it.",
            tools: [
              {
                name: "patch",
                input: { path: "packages/faber-ui/components/hosts/agent-install.tsx" },
                result: "applied 1 hunk",
              },
            ],
          },
          {
            text: "Moved `AgentInstall` to `components/hosts/agent-install.tsx`; the copy button is unchanged.",
          },
        ],
      },
    ],
  })

  addSessionWithRuns(db, {
    id: IDS.sessionEmpty,
    title: null,
    ageSeconds: 5 * 60,
    runs: [],
  })

  db.sessionEnvironments[IDS.sessionRefactor] = [
    {
      label: "laptop",
      host_id: IDS.hostLaptop,
      container_id: null,
      added_at: epochAgo(2 * HOUR),
      removed_at: null,
    },
  ]

  return db
}
