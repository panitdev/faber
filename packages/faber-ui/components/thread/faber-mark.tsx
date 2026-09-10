"use client"

import * as React from "react"
import { motion, useReducedMotion } from "framer-motion"

import faberLogoUrl from "@/assets/brand/generated/faber-logo.png"
import faberLogoWingsUrl from "@/assets/brand/generated/faber-logo-with-wings.png"
import { PANIT_DEFAULT_EASE } from "@/lib/motion"
import { cn } from "@/lib/utils"

/**
 * The Faber mark, idle or in flight.
 *
 * Two artworks, not one: the plain logo (the head) and the winged logo are
 * separate marks. Both are rendered from local generated PNGs on the same
 * square canvas, so the container is fixed and both layers are simply
 * centred in it, crossfading in place. No per-artwork alignment.
 */

/** Both generated marks share a square canvas. */
export const MARK_ASPECT = 1

export function FaberMark({
  className,
  size = 18,
  working = false,
}: {
  className?: string
  /** Height of the mark in pixels; the box is `size * MARK_ASPECT` wide. */
  size?: number
  working?: boolean
}) {
  const reduce = useReducedMotion()
  const transition = reduce
    ? { duration: 0 }
    : { duration: 0.45, ease: PANIT_DEFAULT_EASE }

  return (
    <span
      aria-hidden
      className={cn("relative block shrink-0", className)}
      style={{ width: size * MARK_ASPECT, height: size }}
    >
      <motion.span
        className="absolute inset-0 flex items-center justify-center"
        initial={false}
        animate={{
          opacity: working ? 0 : 1,
          filter: working ? "blur(4px)" : "blur(0px)",
          x: working ? "-25%" : "0%",
        }}
        transition={transition}
        style={{ backfaceVisibility: "hidden", willChange: "transform, filter, opacity" }}
      >
        <img src={faberLogoUrl} alt="" width={size} height={size} aria-hidden />
      </motion.span>

      <motion.span
        className="absolute inset-0 flex items-center justify-center"
        initial={false}
        animate={{
          opacity: working ? 1 : 0,
          filter: working ? "blur(0px)" : "blur(4px)",
          // x: working ? "0%" : "25%",
          scale: working || reduce ? 1 : 0.9,
        }}
        transition={transition}
        style={{ backfaceVisibility: "hidden", willChange: "transform, filter, opacity" }}
      >
        <img src={faberLogoWingsUrl} alt="" width={size} height={size} aria-hidden />
      </motion.span>
    </span>
  )
}
