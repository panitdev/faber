import * as React from "react"

import type { JsonValue } from "@/lib/api"
import { Input } from "@charlotte-ui/input"
import type { BuiltInSettings, SettingsFormProps } from "./plugin-settings"

interface Draft {
  token: string
  channelId: string
}

function read(config: JsonValue): Draft {
  const object = config && typeof config === "object" && !Array.isArray(config) ? config : {}
  const text = (value: JsonValue | undefined) => (typeof value === "string" ? value : "")
  return { token: text(object.token), channelId: text(object.channel_id) }
}

function write(draft: Draft): JsonValue {
  const config: Record<string, JsonValue> = {}
  if (draft.token.trim()) config.token = draft.token.trim()
  if (draft.channelId.trim()) config.channel_id = draft.channelId.trim()
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
      <FieldError message={whole} />
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
