import * as React from "react"

import type { JsonValue } from "@/lib/api"
import { Input } from "@charlotte-ui/input"
import { Switch } from "@charlotte-ui/switch"
import type { BuiltInSettings, SettingsFormProps } from "./plugin-settings"

interface Draft {
  token: string
  channelId: string
  showReasoning: boolean
  /** As typed; empty leaves the plugin's own default. */
  reasoningLimit: string
  showTime: boolean
  showTokens: boolean
  showCost: boolean
}

function read(config: JsonValue): Draft {
  const object = config && typeof config === "object" && !Array.isArray(config) ? config : {}
  const text = (value: JsonValue | undefined) => (typeof value === "string" ? value : "")
  return {
    token: text(object.token),
    channelId: text(object.channel_id),
    showReasoning: object.show_reasoning === true,
    reasoningLimit: typeof object.reasoning_limit === "number" ? String(object.reasoning_limit) : "",
    showTime: object.show_time === true,
    showTokens: object.show_tokens === true,
    showCost: object.show_cost === true,
  }
}

function write(draft: Draft): JsonValue {
  const config: Record<string, JsonValue> = {}
  if (draft.token.trim()) config.token = draft.token.trim()
  if (draft.channelId.trim()) config.channel_id = draft.channelId.trim()
  if (draft.showReasoning) config.show_reasoning = true
  // Sent as typed when it is not a number, so the plugin's own check answers.
  const limit = draft.reasoningLimit.trim()
  if (limit) config.reasoning_limit = /^\d+$/.test(limit) ? Number(limit) : limit
  if (draft.showTime) config.show_time = true
  if (draft.showTokens) config.show_tokens = true
  if (draft.showCost) config.show_cost = true
  return config
}

function DiscordForm({ value, onChange, errors, disabled }: SettingsFormProps<Draft>) {
  const whole = errors.get("")
  return (
    <div className="flex flex-col gap-4">
      <div>
        <Input
          id="discord-token"
          label="Bot token"
          type="password"
          autoComplete="off"
          value={value.token}
          disabled={disabled}
          onChange={(text) => onChange({ ...value, token: text })}
          hint="From Discord's developer portal. Turn on the bot's Message Content intent."
          aria-invalid={errors.has("/token") || undefined}
        />
        <FieldError message={errors.get("/token")} />
      </div>
      <div>
        <Input
          id="discord-channel"
          label="Channel ID"
          value={value.channelId}
          disabled={disabled}
          onChange={(text) => onChange({ ...value, channelId: text })}
          hint="Right-click the channel in Discord and choose Copy Channel ID."
          aria-invalid={errors.has("/channel_id") || undefined}
        />
        <FieldError message={errors.get("/channel_id")} />
      </div>
      <section className="flex flex-col gap-2">
        <Toggle
          label="Show reasoning before answers"
          checked={value.showReasoning}
          disabled={disabled}
          onChange={(on) => onChange({ ...value, showReasoning: on })}
        />
        {value.showReasoning ? (
          <div>
            <Input
              id="discord-reasoning-limit"
              label="Reasoning shown, in characters"
              placeholder="300"
              value={value.reasoningLimit}
              disabled={disabled}
              onChange={(text) => onChange({ ...value, reasoningLimit: text })}
              hint="Longer reasoning is cut and ends in “...”. Posted as small text."
              aria-invalid={errors.has("/reasoning_limit") || undefined}
            />
            <FieldError message={errors.get("/reasoning_limit")} />
          </div>
        ) : null}
      </section>
      <section className="flex flex-col gap-2">
        <p className="text-xs text-muted-foreground">
          After a run&apos;s answer, as small text: (22s, 1.2k/30 tokens, $0.001)
        </p>
        <Toggle
          label="Time to respond"
          checked={value.showTime}
          disabled={disabled}
          onChange={(on) => onChange({ ...value, showTime: on })}
        />
        <Toggle
          label="Input and output tokens"
          checked={value.showTokens}
          disabled={disabled}
          onChange={(on) => onChange({ ...value, showTokens: on })}
        />
        <Toggle
          label="Cost, when the provider reports one"
          checked={value.showCost}
          disabled={disabled}
          onChange={(on) => onChange({ ...value, showCost: on })}
        />
      </section>
      <FieldError message={whole} />
    </div>
  )
}

function Toggle({
  label,
  checked,
  disabled,
  onChange,
}: {
  label: string
  checked: boolean
  disabled?: boolean
  onChange: (on: boolean) => void
}) {
  return (
    <div className="flex items-center gap-3">
      <span className="flex-1 text-sm">{label}</span>
      <Switch size="sm" checked={checked} disabled={disabled} onCheckedChange={onChange} aria-label={label} />
    </div>
  )
}

function FieldError({ message }: { message?: string }) {
  return message ? <p className="mt-1.5 text-xs text-destructive">{message}</p> : null
}

export const discordSettings: BuiltInSettings<Draft> = {
  read,
  write,
  Form: DiscordForm,
  description:
    "Messages in this channel go to one thread of this project, run on your models, and " +
    "the agent's answers are posted back as the bot. Archive the thread to start a fresh " +
    "one. One Discord connection serves every project this plugin is bound to, so every " +
    "bound project's bot token is held in the same process.",
}
