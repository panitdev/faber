"use client"

import * as React from "react"
import { Check } from "lucide-react"

import type { ModelConfig } from "@/lib/api"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@charlotte-ui/dropdown-menu"

export type ModelPickerProps = {
  models: ModelConfig[]
  /** The model in effect — `null` while the list is still empty. */
  selected: ModelConfig | null
  onSelect: (alias: string) => void
  /** False while the model list is still in flight, so "none" isn't claimed early. */
  loaded?: boolean
  disabled?: boolean
  /**
   * Which way the menu opens. `top` suits a composer pinned to the bottom of
   * the screen; one near the top needs `bottom`, or the menu fits above until
   * a taller level slides in and Radix flips it below mid-interaction.
   */
  side?: "top" | "bottom"
  className?: string
}

/**
 * Which model the next message goes to. Reasoning effort is its own control,
 * beside this one, because it is a level to slide along rather than a list to
 * pick from.
 */
export function ModelPicker({
  models,
  selected,
  onSelect,
  loaded = true,
  disabled = false,
  side = "top",
  className,
}: ModelPickerProps) {
  const empty = models.length === 0
  const modelValue = selected?.alias ?? (loaded ? "None" : "…")

  return (
    <DropdownMenu expandTriggerToMenuWidth>
      <DropdownMenuTrigger
        disabled={disabled || empty}
        aria-label="Select model"
        className={className}
      >
        <span>{selected?.alias ?? (loaded ? "No model" : "…")}</span>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side={side} className="min-w-48">
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
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
