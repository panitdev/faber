import * as React from "react"

import { cn } from "@/lib/utils"

/**
 * The Panit mark as a loading indicator.
 *
 * - `loading` false → the static mark.
 * - `loading` true → the mark spins up; while it accelerates, the dome and the bowl each fold
 *   into their own dot, landing on the dots exactly as the spin reaches full speed. The two dots
 *   then orbit indefinitely, their speed gently rising and falling.
 * - `loading` back to false → a single brake from the current speed that stops on a whole turn
 *   (the mark lands upright) in `exitMs`; the dots unfold back into the halves in step with it.
 *
 * The shape change is locked to the rotation (it advances by the share of the spin-up / brake
 * angle turned), so shape and spin always start and finish together. A trail of faint copies,
 * sampled a few ms back in time, gives a motion blur that grows with angular speed.
 * Under `prefers-reduced-motion` the blur trail is turned off.
 */

const TOP =
  "M6.72 57.95 L6.78 58.15 L6.88 58.34 L7.02 58.50 L7.18 58.62 L7.37 58.71 L7.56 58.76 L7.78 58.77 L7.97 58.73 L8.11 58.68 L9.35 58.11 L10.78 57.48 L12.18 56.89 L13.57 56.32 L15.00 55.77 L16.49 55.22 L17.92 54.72 L19.31 54.25 L20.74 53.79 L22.24 53.34 L23.66 52.93 L25.06 52.55 L26.49 52.18 L27.99 51.82 L29.42 51.50 L31.28 51.11 L33.65 50.68 L36.12 50.28 L38.45 49.96 L40.92 49.69 L43.29 49.48 L46.09 49.31 L48.57 49.22 L50.93 49.19 L53.77 49.23 L56.25 49.33 L57.65 49.41 L59.06 49.52 L61.55 49.75 L62.98 49.91 L64.38 50.09 L66.75 50.43 L69.25 50.86 L71.59 51.32 L74.09 51.88 L76.46 52.46 L78.83 53.11 L81.30 53.86 L82.74 54.32 L84.14 54.80 L86.65 55.70 L88.05 56.25 L89.49 56.82 L92.08 57.94 L92.35 57.99 L92.61 57.98 L92.76 57.94 L92.89 57.88 L93.12 57.72 L93.22 57.61 L93.30 57.49 L93.36 57.36 L93.40 57.21 L93.60 55.93 L93.76 54.59 L93.88 53.24 L93.96 51.90 L93.99 50.82 L94.00 49.47 L93.96 48.12 L93.90 47.04 L93.79 45.69 L93.67 44.62 L93.48 43.28 L93.26 41.95 L93.05 40.89 L92.81 39.84 L92.55 38.79 L92.26 37.75 L91.95 36.72 L91.61 35.69 L91.24 34.68 L90.86 33.67 L90.44 32.67 L90.01 31.68 L89.54 30.71 L89.06 29.74 L88.55 28.79 L88.02 27.85 L87.46 26.92 L86.89 26.01 L86.29 25.11 L85.66 24.23 L85.02 23.36 L84.36 22.51 L83.67 21.68 L82.97 20.86 L82.24 20.06 L81.50 19.28 L80.73 18.51 L79.95 17.77 L79.15 17.04 L78.33 16.34 L77.50 15.65 L76.65 14.99 L75.78 14.34 L74.90 13.72 L74.00 13.12 L73.09 12.54 L72.16 11.99 L71.22 11.46 L70.27 10.95 L69.30 10.46 L68.33 10.00 L67.34 9.56 L66.34 9.15 L65.34 8.76 L64.32 8.40 L63.29 8.06 L62.26 7.74 L61.22 7.45 L60.17 7.19 L59.12 6.96 L58.06 6.74 L57.00 6.56 L55.93 6.40 L54.86 6.27 L53.78 6.16 L52.70 6.08 L51.63 6.03 L50.55 6.00 L49.47 6.00 L48.39 6.03 L47.31 6.08 L46.23 6.16 L45.16 6.27 L44.08 6.40 L43.02 6.56 L41.95 6.74 L40.89 6.95 L39.84 7.19 L38.79 7.45 L37.75 7.74 L36.72 8.05 L35.69 8.39 L34.68 8.76 L33.67 9.14 L32.67 9.56 L31.68 9.99 L30.71 10.46 L29.74 10.94 L28.79 11.45 L27.85 11.98 L26.92 12.54 L26.01 13.11 L25.11 13.71 L24.23 14.34 L23.36 14.98 L22.51 15.64 L21.68 16.33 L20.86 17.03 L20.06 17.76 L19.28 18.50 L18.51 19.27 L17.77 20.05 L17.04 20.85 L16.34 21.67 L15.65 22.50 L14.99 23.35 L14.34 24.22 L13.72 25.10 L13.12 26.00 L12.54 26.91 L11.99 27.84 L11.46 28.78 L10.95 29.73 L10.46 30.70 L10.00 31.67 L9.56 32.66 L9.15 33.66 L8.76 34.66 L8.40 35.68 L8.06 36.71 L7.74 37.74 L7.45 38.78 L7.19 39.83 L6.96 40.88 L6.74 41.94 L6.56 43.00 L6.40 44.07 L6.27 45.14 L6.16 46.22 L6.08 47.30 L6.03 48.37 L6.00 49.45 L6.00 50.53 L6.03 51.61 L6.08 52.69 L6.16 53.77 L6.27 54.84 L6.40 55.92 L6.56 56.98Z"
const BOTTOM =
  "M9.09 63.67 L8.91 63.78 L8.77 63.92 L8.66 64.08 L8.59 64.27 L8.55 64.46 L8.55 64.65 L8.58 64.83 L8.66 65.07 L9.04 66.08 L9.45 67.08 L9.88 68.07 L10.34 69.05 L10.82 70.02 L11.32 70.97 L11.85 71.92 L12.54 73.08 L13.41 74.44 L14.34 75.77 L15.31 77.07 L16.33 78.32 L17.58 79.74 L18.69 80.92 L19.85 82.05 L21.05 83.14 L22.08 84.01 L23.35 85.01 L24.66 85.97 L26.00 86.88 L27.37 87.74 L28.78 88.54 L29.97 89.18 L31.43 89.89 L32.66 90.44 L34.16 91.05 L35.68 91.60 L37.22 92.10 L38.52 92.48 L40.09 92.87 L41.68 93.21 L43.27 93.48 L44.88 93.70 L46.49 93.86 L47.84 93.95 L49.45 94.00 L50.80 93.99 L52.42 93.93 L53.77 93.84 L55.38 93.67 L56.98 93.44 L58.31 93.21 L59.90 92.87 L61.21 92.55 L62.77 92.11 L64.31 91.61 L65.58 91.15 L67.08 90.55 L68.32 90.01 L69.78 89.30 L70.97 88.68 L72.38 87.88 L73.76 87.03 L75.11 86.13 L76.21 85.34 L77.49 84.36 L78.74 83.32 L79.94 82.24 L81.11 81.12 L82.23 79.95 L83.31 78.74 L84.52 77.29 L85.42 76.11 L85.52 75.92 L85.58 75.70 L85.59 75.49 L85.55 75.28 L85.46 75.06 L85.34 74.89 L85.19 74.75 L85.01 74.64 L84.80 74.57 L84.59 74.55 L84.43 74.56 L82.79 74.85 L81.35 75.08 L79.90 75.29 L78.69 75.44 L77.25 75.60 L75.83 75.74 L74.41 75.85 L73.02 75.93 L71.63 75.99 L70.25 76.02 L68.89 76.03 L67.55 76.01 L66.23 75.97 L64.94 75.90 L63.67 75.80 L62.60 75.70 L60.58 75.45 L59.60 75.30 L58.65 75.13 L57.71 74.94 L56.80 74.74 L55.91 74.52 L55.08 74.29 L54.24 74.04 L53.44 73.77 L52.66 73.49 L51.91 73.19 L51.19 72.88 L50.49 72.55 L49.83 72.21 L49.20 71.85 L48.49 71.40 L47.83 70.94 L47.27 70.50 L46.69 70.00 L46.20 69.51 L45.72 68.98 L45.32 68.47 L44.93 67.91 L44.63 67.38 L44.35 66.79 L44.11 66.19 L43.94 65.59 L43.81 64.97 L43.74 64.40 L43.71 63.75 L43.73 63.12 L43.80 62.46 L43.92 61.79 L44.06 61.26 L44.26 60.73 L44.50 60.25 L44.84 59.71 L45.19 59.27 L45.56 58.89 L45.95 58.55 L46.28 58.32 L46.64 58.10 L47.00 57.92 L47.60 57.68 L49.18 57.20 L50.58 56.74 L50.82 56.62 L51.02 56.42 L51.16 56.20 L51.23 55.95 L51.23 55.69 L51.17 55.44 L51.05 55.21 L50.87 55.02 L50.64 54.89 L50.39 54.82 L49.74 54.80 L47.95 54.80 L46.58 54.83 L45.21 54.87 L42.48 55.02 L41.12 55.13 L39.77 55.25 L38.41 55.40 L37.04 55.56 L34.32 55.95 L32.03 56.34 L29.32 56.88 L27.95 57.18 L26.59 57.50 L25.22 57.85 L23.85 58.22 L22.48 58.61 L21.12 59.02 L18.84 59.75 L17.47 60.23 L16.11 60.72 L13.84 61.60 L12.45 62.17 L11.08 62.76 L9.72 63.37Z"

const PALETTES = {
  color: { top: "#2F6BFF", bottom: "#0E1F5C" },
  dark: { top: "#FFFFFF", bottom: "#3D78FF" },
  mono: { top: "currentColor", bottom: "currentColor" },
} as const

const GHOSTS = 8 // motion-blur copies behind the mark
const GHOST_GAP = 6 // ms between ghost samples
const POINTS = 140 // points per morphing outline
const DOT_R = 10
const BREATH_MS = 1300 // period of the orbit's speed variation

/* ---------- geometry: each half becomes its own dot ---------- */

type Pt = [number, number]
type Shape = { s: Pt[]; ang: number[]; cx: number; cy: number; d0: string }

function parsePoly(d: string): Pt[] {
  const nums = (d.match(/-?\d+(\.\d+)?/g) ?? []).map(Number)
  const pts: Pt[] = []
  for (let i = 0; i + 1 < nums.length; i += 2) pts.push([nums[i], nums[i + 1]])
  return pts
}

function signedArea(p: Pt[]) {
  let a = 0
  for (let i = 0; i < p.length; i++) {
    const q = p[(i + 1) % p.length]
    a += p[i][0] * q[1] - q[0] * p[i][1]
  }
  return a / 2
}

function resample(poly: Pt[], n: number): Pt[] {
  const p = signedArea(poly) < 0 ? poly.slice().reverse() : poly
  const cum = [0]
  let total = 0
  for (let i = 0; i < p.length; i++) {
    const q = p[(i + 1) % p.length]
    total += Math.hypot(q[0] - p[i][0], q[1] - p[i][1])
    cum.push(total)
  }
  const out: Pt[] = []
  let j = 0
  for (let k = 0; k < n; k++) {
    const s = (k / n) * total
    while (cum[j + 1] < s) j++
    const a = p[j]
    const b = p[(j + 1) % p.length]
    const t = (s - cum[j]) / (cum[j + 1] - cum[j] || 1)
    out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t])
  }
  return out
}

/** Matches every outline point to a point on the target dot, choosing the circle phase that
 *  keeps the total travel shortest. */
function buildShape(d: string, cx: number, cy: number): Shape {
  const s = resample(parsePoly(d), POINTS)
  let mx = 0
  let my = 0
  for (const p of s) {
    mx += p[0]
    my += p[1]
  }
  mx /= POINTS
  my /= POINTS
  let best = 0
  let bestCost = Infinity
  for (let off = 0; off < POINTS; off++) {
    let cost = 0
    for (let k = 0; k < POINTS; k++) {
      const a = -Math.PI / 2 + (2 * Math.PI * ((k + off) % POINTS)) / POINTS
      const dx = s[k][0] - mx - Math.cos(a) * 25
      const dy = s[k][1] - my - Math.sin(a) * 25
      cost += dx * dx + dy * dy
    }
    if (cost < bestCost) {
      bestCost = cost
      best = off
    }
  }
  const ang: number[] = []
  for (let k = 0; k < POINTS; k++) {
    ang.push(-Math.PI / 2 + (2 * Math.PI * ((k + best) % POINTS)) / POINTS)
  }
  return { s, ang, cx, cy, d0: d }
}

let shapes: { top: Shape; bottom: Shape } | null = null
function getShapes() {
  shapes ??= { top: buildShape(TOP, 50, 34), bottom: buildShape(BOTTOM, 50, 66) }
  return shapes
}

const clamp01 = (x: number) => (x < 0 ? 0 : x > 1 ? 1 : x)
const smooth = (x: number) => {
  const c = clamp01(x)
  return c * c * (3 - 2 * c)
}

/** m: 0 = logo half, 1 = dot. */
function outline(sh: Shape, m: number) {
  if (m <= 0.0005) return sh.d0
  let out = ""
  for (let k = 0; k < POINTS; k++) {
    // a light per-point lead/lag so the shape pours rather than shrinks uniformly
    const lag = 0.16 * (0.5 + 0.5 * Math.cos(sh.ang[k] + Math.PI / 2))
    const u = clamp01(m * (1 + lag) - lag * m * m)
    const tx = sh.cx + Math.cos(sh.ang[k]) * DOT_R
    const ty = sh.cy + Math.sin(sh.ang[k]) * DOT_R
    const x = sh.s[k][0] + (tx - sh.s[k][0]) * u
    const y = sh.s[k][1] + (ty - sh.s[k][1]) * u
    out += `${k ? "L" : "M"}${x.toFixed(2)} ${y.toFixed(2)}`
  }
  return out + "Z"
}

/* ---------- timeline ---------- */

type Sample = { t: number; th: number; m: number; w: number }
type Exit = { t: number; th: number; D: number; T: number; n: number }
type Engine = {
  phase: "static" | "in" | "loop" | "out"
  t0: number | null
  last: number
  tm: number
  th: number // unwrapped rotation, degrees
  w: number // angular speed, degrees / ms
  m: number // morph, 0 = mark, 1 = dots
  exit: Exit | null
  hist: Sample[]
  raf: number
  // per-run settings, fixed when a run starts
  T_IN: number
  T_X: number
  W0: number
}

export type BrandLoaderProps = Omit<React.ComponentProps<"svg">, "children"> & {
  /** Show the loading animation. Setting it back to false lands the mark upright. */
  loading?: boolean
  /** Rendered width and height in px. */
  size?: number
  /** `color` on light backgrounds, `dark` on dark ones, `mono` follows `currentColor`. */
  variant?: keyof typeof PALETTES
  /** Spin-up time; the halves become dots over exactly this span. */
  transitionMs?: number
  /** Brake time when loading ends; the dots become the mark over exactly this span. */
  exitMs?: number
  /** Orbit speed in degrees per millisecond. */
  spin?: number
  /** Motion-blur strength, 0–2. 0 turns the trail off. */
  blur?: number
  /** Called when the mark has landed after loading ends. */
  onSettled?: () => void
}

export function BrandLoader({
  loading = false,
  size = 32,
  variant = "color",
  transitionMs = 800,
  exitMs = 550,
  spin = 1,
  blur = 1,
  onSettled,
  className,
  ...props
}: BrandLoaderProps) {
  const palette = PALETTES[variant]
  const layerRefs = React.useRef<(SVGGElement | null)[]>([])
  const latest = React.useRef({ loading, transitionMs, exitMs, spin, blur, onSettled })
  latest.current = { loading, transitionMs, exitMs, spin, blur, onSettled }

  const engine = React.useRef<Engine>({
    phase: "static",
    t0: null,
    last: 0,
    tm: 0,
    th: 0,
    w: 0,
    m: 0,
    exit: null,
    hist: [],
    raf: 0,
    T_IN: transitionMs,
    T_X: exitMs,
    W0: spin,
  })

  const paint = React.useCallback((now: number) => {
    const e = engine.current
    const { top, bottom } = getShapes()
    const at = (t: number): Omit<Sample, "t"> => {
      const h = e.hist
      if (!h.length) return { th: 0, m: 0, w: 0 }
      if (t <= h[0].t) return h[0]
      for (let j = h.length - 1; j > 0; j--) {
        if (h[j - 1].t <= t) {
          const a = h[j - 1]
          const b = h[j]
          const f = (t - a.t) / (b.t - a.t || 1)
          return { th: a.th + (b.th - a.th) * f, m: a.m + (b.m - a.m) * f, w: a.w + (b.w - a.w) * f }
        }
      }
      return h[h.length - 1]
    }
    let reduce = false
    try {
      reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches
    } catch {
      // matchMedia unavailable; keep motion on.
    }
    const B = reduce ? 0 : latest.current.blur
    const cur = at(now)
    const strength = clamp01(Math.abs(cur.w) / 1.1) * B
    const cache = new Map<string, [string, string]>()
    for (let i = 0; i <= GHOSTS; i++) {
      const g = layerRefs.current[i]
      if (!g) continue
      const s = i === 0 ? cur : at(now - i * GHOST_GAP)
      const key = s.m.toFixed(3)
      let d = cache.get(key)
      if (!d) {
        d = [outline(top, s.m), outline(bottom, s.m)]
        cache.set(key, d)
      }
      g.setAttribute("transform", `rotate(${(s.th % 360).toFixed(2)} 50 50)`)
      if (i > 0) {
        g.setAttribute("opacity", (strength * 0.4 * Math.pow(1 - i / (GHOSTS + 1), 1.4)).toFixed(3))
      }
      g.children[0]?.setAttribute("d", d[0])
      g.children[1]?.setAttribute("d", d[1])
    }
  }, [])

  const step = React.useCallback(
    (now: number) => {
      const e = engine.current
      if (e.t0 === null) {
        e.t0 = now
        e.last = now
        e.hist = [{ t: now - 1, th: 0, m: 0, w: 0 }]
      }
      const dt = now - e.last
      e.last = now
      let u = 0 // brake progress; 0 on the frame the brake begins

      if (e.phase === "in") {
        // spin up from rest; the shape follows the share of the spin-up angle turned (tau²)
        const tau = clamp01((now - e.t0) / e.T_IN)
        e.th = 0.5 * e.W0 * e.T_IN * tau * tau
        e.w = e.W0 * tau
        e.m = smooth(tau * tau)
        if (tau >= 1) {
          e.phase = "loop"
          e.tm = now
        }
      } else if (e.phase === "loop") {
        // indeterminate: orbit, speed breathing around the set speed
        e.w = e.W0 * (1 + 0.35 * Math.sin((2 * Math.PI * (now - e.tm)) / BREATH_MS))
        e.th += e.w * dt
        e.m = 1
        if (!latest.current.loading) {
          // Brake from exactly the current speed along an ease-out of power n, chosen so the spin
          // stops on a whole turn in T_X. n stays within 1.6–3.2 (below it barely slows, above it
          // stops abruptly); if no whole turn fits right now, orbit a few more degrees.
          const base = Math.ceil(e.th / 360) * 360 - e.th
          for (let c = 0; c < 3 && e.phase === "loop"; c++) {
            const D = base + 360 * c
            const n = (e.w * e.T_X) / D
            if (D > 20 && n >= 1.6 && n <= 3.2) {
              e.phase = "out"
              e.exit = { t: now, th: e.th, D, T: e.T_X, n }
            }
          }
        }
      } else if (e.phase === "out" && e.exit) {
        const x = e.exit
        u = clamp01((now - x.t) / x.T)
        e.th = x.th + x.D * (1 - Math.pow(1 - u, x.n))
        e.w = ((x.n * x.D) / x.T) * Math.pow(1 - u, x.n - 1)
      }

      if (e.phase === "out" && e.exit) {
        // the shape follows the share of the brake angle turned: mark and stop arrive together
        e.m = 1 - smooth(1 - Math.pow(1 - u, e.exit.n))
        if (u >= 1) {
          e.phase = "static"
          e.th = 0
          e.w = 0
          e.m = 0
          e.exit = null
          e.hist = [{ t: now, th: 0, m: 0, w: 0 }]
          paint(now)
          latest.current.onSettled?.()
          if (latest.current.loading) start()
          return
        }
      }

      e.hist.push({ t: now, th: e.th, m: e.m, w: e.w })
      if (e.hist.length > 40) e.hist.shift()
      paint(now)
      e.raf = requestAnimationFrame(step)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [paint]
  )

  const start = React.useCallback(() => {
    const e = engine.current
    const o = latest.current
    cancelAnimationFrame(e.raf)
    Object.assign(e, {
      phase: "in",
      t0: null,
      th: 0,
      w: 0,
      m: 0,
      exit: null,
      T_IN: o.transitionMs,
      T_X: o.exitMs,
      W0: o.spin,
    })
    e.raf = requestAnimationFrame(step)
  }, [step])

  React.useEffect(() => {
    // Starting only from rest; a stop is picked up by the running loop, and a restart requested
    // mid-landing runs as soon as the mark has landed.
    if (loading && engine.current.phase === "static") start()
  }, [loading, start])

  React.useEffect(() => {
    const e = engine.current
    return () => {
      // Reset on teardown so the next setup can restart from rest. React
      // StrictMode runs setup → cleanup → setup in dev, and without this the
      // second setup sees `phase !== "static"` (the first `start()` already
      // moved it to "in") and skips `start()`, leaving the loop dead.
      cancelAnimationFrame(e.raf)
      e.phase = "static"
      e.t0 = null
    }
  }, [])

  const layers = []
  for (let i = GHOSTS; i >= 0; i--) {
    layers.push(
      <g
        key={i}
        ref={(el) => {
          layerRefs.current[i] = el
        }}
        opacity={i === 0 ? 1 : 0}
      >
        {/* ghost copies get their outlines from the animation; only the mark itself ships them */}
        <path d={i === 0 ? TOP : undefined} fill={palette.top} />
        <path d={i === 0 ? BOTTOM : undefined} fill={palette.bottom} />
      </g>
    )
  }

  return (
    <svg
      role="img"
      aria-label={loading ? "Loading" : "Panit"}
      aria-busy={loading || undefined}
      viewBox="0 0 100 100"
      width={size}
      height={size}
      className={cn("shrink-0 overflow-visible", className)}
      {...props}
    >
      {layers}
    </svg>
  )
}
