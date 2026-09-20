"use client"

import * as React from "react"
import { Squircle } from "@squircle-js/react"

import { cn } from "@/lib/utils"

/**
 * True where the browser draws `corner-shape: squircle` natively (Chromium
 * 139+ as of 2026). Everywhere else the frame falls back to the
 * `@squircle-js/react` SVG clip-path. The check runs once as a state
 * initialiser so there is no extra render pass.
 */
function supportsNativeSquircle() {
  return (
    typeof CSS !== "undefined" &&
    typeof CSS.supports === "function" &&
    CSS.supports("corner-shape: squircle")
  )
}

export type SquircleFrameProps = {
  children: React.ReactNode
  /** Extra classes for the outer ring layer (shadow, transitions, layout). */
  className?: string
  /** Classes for the inner face layer (background, blur). */
  faceClassName?: string
  style?: React.CSSProperties
  faceStyle?: React.CSSProperties
  /** Outer corner radius in px. Defaults to 16. */
  cornerRadius?: number
  /**
   * Ring width in px. The ring is a slightly larger solid squircle behind
   * the face — a real CSS border cannot be stroked along the fallback
   * clip-path, but this reads exactly like one. Pass 0 for a borderless
   * single face.
   */
  borderWidth?: number
  /** 0 (round arcs) to 1 (full smoothing). Defaults to 0.6, iOS standard. */
  cornerSmoothing?: number
  /** Background of the ring — the "border color". Defaults to `bg-input`. */
  borderClassName?: string
}

/**
 * A squircle with a border that survives every browser.
 *
 * Native `corner-shape: squircle` where supported, nested SVG clip-paths
 * elsewhere. The inner face radius is one border-width smaller so the two
 * curves run concentric. Unsupported browsers ignore the unknown
 * `corner-shape` property and keep the plain `border-radius` until the
 * clip-path is measured.
 */
export function SquircleFrame({
  children,
  className,
  faceClassName,
  style,
  faceStyle,
  cornerRadius = 16,
  borderWidth = 1,
  cornerSmoothing = 0.6,
  borderClassName = "bg-input",
}: SquircleFrameProps) {
  const [nativeSquircle] = React.useState(supportsNativeSquircle)
  const width = Math.max(0, borderWidth)
  const innerRadius = Math.max(0, cornerRadius - width)

  // Borderless: a single face needs no ring layer.
  if (width <= 0) {
    const faceCls = cn("[corner-shape:squircle]", faceClassName, className)
    return nativeSquircle ? (
      <div className={faceCls} style={{ borderRadius: cornerRadius, ...faceStyle }}>
        {children}
      </div>
    ) : (
      <Squircle
        cornerRadius={cornerRadius}
        cornerSmoothing={cornerSmoothing}
        className={faceCls}
        style={faceStyle}
      >
        {children}
      </Squircle>
    )
  }

  const frameCls = cn("[corner-shape:squircle]", borderClassName, className)
  const faceCls = cn("[corner-shape:squircle]", faceClassName)

  return nativeSquircle ? (
    <div
      className={frameCls}
      style={{ borderRadius: cornerRadius, padding: width, ...style }}
    >
      <div className={faceCls} style={{ borderRadius: innerRadius, ...faceStyle }}>
        {children}
      </div>
    </div>
  ) : (
    <Squircle
      cornerRadius={cornerRadius}
      cornerSmoothing={cornerSmoothing}
      className={frameCls}
      style={{ padding: width, ...style }}
    >
      <Squircle
        cornerRadius={innerRadius}
        cornerSmoothing={cornerSmoothing}
        className={faceCls}
        style={faceStyle}
      >
        {children}
      </Squircle>
    </Squircle>
  )
}
