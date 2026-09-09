"use client"

import * as React from "react"
import { Check, ChevronsUpDown } from "lucide-react"

import { cn } from "@/lib/utils"
import type { ThinkingCapability, ThinkingSelection } from "@/lib/api"
import { selectionLabel, selectionsFor } from "@/lib/models/thinking"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

export type ThinkingPickerProps = {
  /** The selected model's own declaration — what this picker is allowed to offer. */
  capability: ThinkingCapability
  /** The session's pick, or `null` for the model's own default. */
  selected: ThinkingSelection | null
  onSelect: (selection: ThinkingSelection | null) => void
  disabled?: boolean
  className?: string
}

/**
 * How hard the next message thinks, chosen from the prompt box footer beside
 * the model.
 *
 * Renders nothing at all when the model says it doesn't reason: an inert knob
 * reads as a broken one, and which knobs exist is the model definition's
 * answer to give.
 */
export function ThinkingPicker({
  capability,
  selected,
  onSelect,
  disabled = false,
  className,
}: ThinkingPickerProps) {
  const options = selectionsFor(capability)
  if (options.length === 0) return null

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          disabled={disabled}
          aria-label="Select thinking effort"
          className={cn(
            "flex min-w-0 items-center gap-1 rounded-lg px-2 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50",
            className,
          )}
        >
          <span className="truncate">{selectionLabel(selected)}</span>
          <ChevronsUpDown className="size-3.5 shrink-0 opacity-60" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="min-w-48">
        {/* First, and always offered: clearing the knob is a state of its own —
            it hands the answer back to the model's definition rather than
            standing for any one level. */}
        <DropdownMenuItem
          onSelect={() => onSelect(null)}
          className="flex items-center justify-between gap-3"
        >
          <span className="min-w-0 truncate">{selectionLabel(null)}</span>
          {selected === null ? <Check className="size-4 shrink-0" /> : null}
        </DropdownMenuItem>
        {options.map((option) => (
          <DropdownMenuItem
            key={option}
            onSelect={() => onSelect(option)}
            className="flex items-center justify-between gap-3"
          >
            <span className="min-w-0 truncate">{selectionLabel(option)}</span>
            {option === selected ? <Check className="size-4 shrink-0" /> : null}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
