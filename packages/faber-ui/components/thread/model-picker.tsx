"use client"

import * as React from "react"
import { Check } from "lucide-react"

import type { ModelConfig, ThinkingSelection } from "@/lib/api"
import { selectionLabel, selectionsFor, thinkingOf } from "@/lib/models/thinking"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

export type ModelPickerProps = {
  models: ModelConfig[]
  /** The model in effect — `null` while the list is still empty. */
  selected: ModelConfig | null
  onSelect: (alias: string) => void
  /** False while the model list is still in flight, so "none" isn't claimed early. */
  loaded?: boolean
  /** The session's thinking pick, or `null` for the model's own default. */
  selectedThinking?: ThinkingSelection | null
  /**
   * Provided only when the caller wants a thinking knob at all. The options
   * come from the selected model's own declaration, so a model that doesn't
   * reason simply gets no thinking row.
   */
  onThinkingSelect?: (selection: ThinkingSelection | null) => void
  disabled?: boolean
  className?: string
}

/**
 * Which model the next message goes to, and how hard it thinks — one control
 * whose menu branches into a model list and a thinking list.
 *
 * The thinking row is the old standalone knob folded in: it is offered on the
 * same terms, which is to say not at all when the model says it doesn't reason,
 * and its options are read from the selected model's own definition.
 */
export function ModelPicker({
  models,
  selected,
  onSelect,
  loaded = true,
  selectedThinking = null,
  onThinkingSelect,
  disabled = false,
  className,
}: ModelPickerProps) {
  const empty = models.length === 0
  const thinkingOptions = onThinkingSelect ? selectionsFor(thinkingOf(selected)) : []
  const modelValue = selected?.alias ?? (loaded ? "None" : "…")
  // Only shown once a level has actually been picked: the model's own default
  // is not a value, and printing one would invent a choice nobody made. "Off"
  // is a real pick, but it reads as the absence of thinking, so the trigger
  // stays a model name rather than annotating it with the nothing that happens.
  const thinkingHint =
    selectedThinking !== null &&
    selectedThinking !== "off" &&
    thinkingOptions.length > 0
      ? selectionLabel(selectedThinking)
      : null

  return (
    <DropdownMenu expandTriggerToMenuWidth>
      <DropdownMenuTrigger
        disabled={disabled || empty}
        aria-label="Select model and thinking"
        className={className}
      >
        <span>{selected?.alias ?? (loaded ? "No model" : "…")}</span>
        {thinkingHint ? (
          <span className="text-muted-foreground">{thinkingHint}</span>
        ) : null}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="min-w-48">
        <DropdownMenuSub key="model">
          <DropdownMenuSubTrigger>
            <span className="shrink-0">Model</span>
            <span className="ml-auto min-w-0 truncate text-xs text-muted-foreground">
              {modelValue}
            </span>
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="min-w-48">
            {models.map((model) => (
              <DropdownMenuItem
                key={model.id}
                onSelect={() => onSelect(model.alias)}
                className="flex items-center justify-between gap-3"
              >
                <span className="min-w-0">
                  <span className="block truncate">{model.alias}</span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {model.wire_id}
                  </span>
                </span>
                {model.id === selected?.id ? <Check className="size-4 shrink-0" /> : null}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>

        {thinkingOptions.length > 0 ? (
          <DropdownMenuSub key="thinking">
            <DropdownMenuSubTrigger>
              <span className="shrink-0">Thinking</span>
              <span className="ml-auto min-w-0 truncate text-xs text-muted-foreground">
                {selectionLabel(selectedThinking)}
              </span>
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="min-w-48">
              {/* First, and always offered: clearing the knob is a state of its
                  own — it hands the answer back to the model's definition
                  rather than standing for any one level. */}
              <DropdownMenuItem
                onSelect={() => onThinkingSelect?.(null)}
                className="flex items-center justify-between gap-3"
              >
                <span className="min-w-0 truncate">{selectionLabel(null)}</span>
                {selectedThinking === null ? <Check className="size-4 shrink-0" /> : null}
              </DropdownMenuItem>
              {thinkingOptions.map((option) => (
                <DropdownMenuItem
                  key={option}
                  onSelect={() => onThinkingSelect?.(option)}
                  className="flex items-center justify-between gap-3"
                >
                  <span className="min-w-0 truncate">{selectionLabel(option)}</span>
                  {option === selectedThinking ? <Check className="size-4 shrink-0" /> : null}
                </DropdownMenuItem>
              ))}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
