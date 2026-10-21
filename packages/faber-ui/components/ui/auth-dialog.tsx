"use client"

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react"
import {
  AnimatePresence,
  animate,
  motion,
  useAnimationControls,
  useMotionValue,
} from "framer-motion"
import {
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  AtSign,
  ChevronRight,
  Ellipsis,
  Eye,
  EyeOff,
  Lock,
  Pencil,
  ShieldCheck,
  SquareAsterisk,
  User,
} from "lucide-react"
import type {
  FlowResult,
  SurgeClient,
} from "@panit/surge-client"
import { SurgeError } from "@panit/surge-client"

import { Input } from "@charlotte-ui/input"
import { Button } from "@charlotte-ui/button"
import { TotpInput } from "@/components/ui/totp-input"
import { PassphraseInput } from "@/components/ui/passphrase-input"
import {
  Dialog,
  DialogContent,
  DialogTitle,
} from "@charlotte-ui/dialog"
import { cn } from "@charlotte-ui/utils"

// ─── Constants ───────────────────────────────────────────────────────────────

const EASE = [0.22, 1, 0.36, 1] as const
const TRANSITION_DURATION = 0.25
const PASSPHRASE_WORD_COUNT = 6

// ─── Types ───────────────────────────────────────────────────────────────────

export type AuthDialogView = "login" | "register" | "reset-password"

type LoginSubView = "form" | "options" | "totp" | "passphrase"
type AuthStage = 1 | 2

export type AuthDialogProps = {
  surgeClient: SurgeClient
  open: boolean
  onOpenChange?: (open: boolean) => void
  onAuthenticated: (result: FlowResult) => void
  defaultView?: AuthDialogView
  /** Brand mark rendered above the heading. */
  mark?: ReactNode
  /** EFF wordlist for passphrase login/recovery. Omit to hide passphrase options. */
  wordlist?: string[]
  /** Allow closing the dialog without authenticating (default: false). */
  dismissible?: boolean
}

// ─── Error helpers ───────────────────────────────────────────────────────────

type SurgeErrorCode =
  | "invalid_credentials"
  | "username_taken"
  | "identity_disabled"
  | "rate_limited"
  | "validation_error"
  | "invalid_token"
  | (string & {})

export function getSurgeErrorMessage(error: unknown): string {
  if (error instanceof SurgeError) return messageForCode(error.code, error)
  if (typeof error === "string") return messageForCode(error)
  return error instanceof Error
    ? error.message
    : "Something went wrong. Please try again."
}

function messageForCode(code: SurgeErrorCode, error?: SurgeError): string {
  switch (code) {
    case "invalid_credentials":
      return "Incorrect username or password."
    case "username_taken":
      return "That username is already taken."
    case "identity_disabled":
      return "This account has been disabled."
    case "rate_limited":
      return error?.retryAfter
        ? `Too many attempts. Try again in ${error.retryAfter}s.`
        : "Too many attempts. Please try again shortly."
    case "validation_error":
      return error?.message || "Please check your input and try again."
    case "invalid_token":
      return "Your session expired. Please try again."
    default:
      return error?.message || "Something went wrong. Please try again."
  }
}

// ─── Password scoring ────────────────────────────────────────────────────────

function pwScore(password: string) {
  let score = 0
  if (password.length >= 8) score += 1
  if (/[A-Z]/.test(password)) score += 1
  if (/[0-9]/.test(password)) score += 1
  if (/[^A-Za-z0-9]/.test(password)) score += 1
  if (password.length >= 12) score += 1
  return score
}

const PW_LABEL = ["Too short", "Weak", "Okay", "Strong", "Very strong"]

// ─── Animation: HeightAnimationWrapper ──────────────────────────────────────

function useHeightMeasurement() {
  const ref = useRef<HTMLDivElement>(null)
  const [height, setHeight] = useState<number | null>(null)

  useEffect(() => {
    const el = ref.current
    if (!el) return

    const measure = () => {
      if (el.scrollHeight > 0) setHeight(el.scrollHeight)
    }

    const observer = new ResizeObserver(measure)
    observer.observe(el)
    measure()

    return () => observer.disconnect()
  }, [])

  return { ref, height }
}

/**
 * Animates its container's height to the measured content height.
 *
 * The `overflow: hidden` has to sit on the animated element itself — it is what
 * hides the excess while the box is mid-tween — so it clips at exactly the box
 * being animated. That makes the wrapper's width a contract: **the dialog's
 * padding belongs inside this element, not on an ancestor.** Fields paint a
 * `ring-4` focus ring outside their border box, and with the padding outside the
 * clip the box hugs the fields and shaves every ring. `display: flow-root` on
 * the measured child keeps that inner inset inside `scrollHeight`.
 */
function HeightAnimationWrapper({
  children,
  className,
}: {
  children: ReactNode
  className?: string
}) {
  const { ref, height } = useHeightMeasurement()

  return (
    <motion.div
      className={className}
      animate={{ height: height ?? "auto" }}
      transition={{ duration: TRANSITION_DURATION, ease: EASE }}
      style={{ overflow: "hidden" }}
    >
      <div ref={ref} style={{ display: "flow-root" }}>{children}</div>
    </motion.div>
  )
}

// ─── Animation: CrossFade ────────────────────────────────────────────────────

function CrossFade({
  viewKey,
  children,
  className,
}: {
  viewKey: string
  children: ReactNode
  className?: string
}) {
  return (
    <AnimatePresence mode="wait" initial={false}>
      <motion.div
        key={viewKey}
        className={className}
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{
          duration: TRANSITION_DURATION,
          ease: EASE,
          delay: TRANSITION_DURATION,
        }}
      >
        {children}
      </motion.div>
    </AnimatePresence>
  )
}

// ─── Animation: useAuthStageTransition ───────────────────────────────────────

const STAGE_VISIBLE = { opacity: 1 } as const
const STAGE_HIDDEN = { opacity: 0 } as const

function useAuthStageTransition(
  options: { initialStage?: AuthStage } = {},
) {
  const { initialStage = 1 } = options

  const [stage, setStage] = useState<AuthStage>(initialStage)
  const stage1Controls = useAnimationControls()
  const stage2Controls = useAnimationControls()
  const mountedRef = useRef(true)
  const stageRef = useRef(stage)
  const transitioningRef = useRef(false)

  useEffect(() => {
    stageRef.current = stage
  }, [stage])

  useEffect(() => {
    mountedRef.current = true
    stage1Controls.set(initialStage === 1 ? STAGE_VISIBLE : STAGE_HIDDEN)
    stage2Controls.set(initialStage === 2 ? STAGE_VISIBLE : STAGE_HIDDEN)

    return () => {
      mountedRef.current = false
    }
  }, [initialStage, stage1Controls, stage2Controls])

  async function goToStage(nextStage: AuthStage) {
    const currentStage = stageRef.current
    if (nextStage === currentStage || transitioningRef.current) return

    transitioningRef.current = true

    const currentControls = currentStage === 1 ? stage1Controls : stage2Controls
    const nextControls = nextStage === 1 ? stage1Controls : stage2Controls

    await currentControls.start({
      ...STAGE_HIDDEN,
      transition: { duration: TRANSITION_DURATION, ease: EASE, delay: TRANSITION_DURATION },
    })

    if (!mountedRef.current) return

    nextControls.set(STAGE_HIDDEN)
    setStage(nextStage)

    await nextControls.start({
      ...STAGE_VISIBLE,
      transition: { duration: TRANSITION_DURATION, ease: EASE, delay: TRANSITION_DURATION },
    })

    if (!mountedRef.current) return
    transitioningRef.current = false
  }

  return {
    stage,
    goToStage,
    getStageProps(stageId: AuthStage) {
      return {
        animate: stageId === 1 ? stage1Controls : stage2Controls,
        "aria-hidden": stage !== stageId,
        className:
          stage === stageId
            ? "relative z-[1] w-full"
            : "absolute inset-0 z-0 w-full opacity-0 pointer-events-none overflow-hidden",
        inert: stage !== stageId,
        initial: false as const,
      }
    },
  }
}

// ─── Internal: AuthHeading ───────────────────────────────────────────────────

function AuthHeading({
  title,
  description,
  mark,
}: {
  title: ReactNode
  description?: ReactNode
  mark?: ReactNode
}) {
  return (
    <div
      className={cn(
        "flex flex-col items-center gap-3 text-center",
        description ? "mb-6" : "mb-3",
      )}
    >
      {mark ? (
        <span className="inline-flex size-11 items-center justify-center overflow-hidden rounded-xl">
          {mark}
        </span>
      ) : null}
      <h2 className="m-0 font-serif text-[26px] font-medium leading-tight [text-wrap:balance]">
        {title}
      </h2>
      {description ? (
        <p className="m-0 text-sm leading-relaxed text-muted-foreground [text-wrap:pretty]">
          {description}
        </p>
      ) : null}
    </div>
  )
}

// ─── Internal: PasswordField ─────────────────────────────────────────────────

function PasswordField({
  id,
  name,
  label,
  value,
  onChange,
  placeholder,
  autoComplete,
  validate,
  required,
  hint,
  capsHint = true,
  labelAction,
}: {
  id: string
  name?: string
  label: string
  value: string
  onChange: (value: string) => void
  placeholder: string
  autoComplete?: string
  validate?: (value: string) => string | null
  required?: boolean
  hint?: string
  capsHint?: boolean
  labelAction?: ReactNode
}) {
  const [focused, setFocused] = useState(false)
  const [visible, setVisible] = useState(false)
  const [caps, setCaps] = useState(false)

  const handleKey = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (typeof event.getModifierState === "function") {
      setCaps(event.getModifierState("CapsLock"))
    }
  }

  return (
    <div>
      <Input
        id={id}
        name={name}
        label={label}
        type={visible ? "text" : "password"}
        icon={<Lock size={16} />}
        required={required}
        value={value}
        onChange={onChange}
        autoComplete={autoComplete}
        placeholder={placeholder}
        validate={validate}
        hint={hint}
        labelAction={labelAction}
        onKeyDown={handleKey}
        onKeyUp={handleKey}
        onFocus={() => setFocused(true)}
        onBlur={() => {
          setFocused(false)
          setCaps(false)
        }}
        trailing={
          <button
            type="button"
            className="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:text-foreground"
            onClick={() => setVisible((c) => !c)}
            aria-label={visible ? "Hide password" : "Show password"}
          >
            {visible ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        }
      />
      {capsHint && caps && focused ? (
        <div className="mt-1.5 inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2.5 py-1 text-[11px] font-medium text-amber-600">
          <AlertCircle size={11} /> Caps Lock is on
        </div>
      ) : null}
    </div>
  )
}

// ─── Internal: StrengthMeter ─────────────────────────────────────────────────

function StrengthMeter({ score }: { score: number }) {
  return (
    <div className="-mt-1">
      <div className="mt-1.5 flex gap-1">
        {[0, 1, 2, 3, 4].map((i) => (
          <span
            key={i}
            className={cn(
              "h-[3px] flex-1 rounded-full",
              i < score
                ? score <= 1
                  ? "bg-destructive"
                  : score === 2
                    ? "bg-accent"
                    : "bg-primary"
                : "bg-border",
            )}
          />
        ))}
      </div>
      <p className="mt-1.5 font-mono text-[11px] text-muted-foreground">
        {PW_LABEL[Math.max(0, score - 1)] ?? PW_LABEL[0]}
      </p>
    </div>
  )
}

// ─── Internal: IdentityBadge ─────────────────────────────────────────────────

function IdentityBadge({
  username,
  onEdit,
}: {
  username: string
  onEdit: () => void
}) {
  const [hovered, setHovered] = useState(false)
  const buttonRef = useRef<HTMLButtonElement>(null)
  const spacerRef = useRef<HTMLSpanElement>(null)
  const width = useMotionValue(0)
  const hasMeasured = useRef(false)
  const widthAnimation = useRef<ReturnType<typeof animate> | null>(null)

  useLayoutEffect(() => {
    if (!buttonRef.current || !spacerRef.current) return
    const cs = getComputedStyle(buttonRef.current)
    const w =
      spacerRef.current.offsetWidth +
      parseFloat(cs.paddingLeft) +
      parseFloat(cs.paddingRight)
    if (!hasMeasured.current) {
      width.jump(w)
      hasMeasured.current = true
    } else {
      widthAnimation.current?.stop()
      widthAnimation.current = animate(width, w, {
        type: "spring",
        stiffness: 350,
        damping: 35,
      })
    }
  }, [hovered, username, width])

  return (
    <motion.button
      ref={buttonRef}
      type="button"
      style={{ width }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onClick={onEdit}
      className="relative mx-auto flex h-8 cursor-pointer items-center gap-1.5 overflow-hidden rounded-full border border-border py-1 pr-4 pl-1 text-sm transition-colors hover:bg-muted"
    >
      <span
        ref={spacerRef}
        aria-hidden
        className="pointer-events-none invisible inline-flex shrink-0 items-center gap-1.5"
      >
        {hovered ? (
          <>
            <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
              <Pencil size={14} strokeWidth={2} />
            </span>
            Change
          </>
        ) : (
          <>
            <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
              <AtSign size={14} strokeWidth={2} />
            </span>
            {username}
          </>
        )}
      </span>
      <AnimatePresence mode="popLayout" initial={false}>
        {hovered ? (
          <motion.span
            key="change"
            initial={{ y: "100%" }}
            animate={{ y: 0 }}
            exit={{ y: "100%" }}
            transition={{ type: "spring", stiffness: 350, damping: 18 }}
            className="absolute inset-0 inline-flex items-center justify-center gap-1.5"
          >
            <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
              <Pencil size={14} strokeWidth={2} />
            </span>
            Change
          </motion.span>
        ) : (
          <motion.span
            key="username"
            initial={{ y: "-100%" }}
            animate={{ y: 0 }}
            exit={{ y: "-100%" }}
            transition={{ type: "spring", stiffness: 350, damping: 18 }}
            className="absolute inset-0 inline-flex items-center justify-center gap-1.5"
          >
            <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
              <AtSign size={14} strokeWidth={2} />
            </span>
            {username}
          </motion.span>
        )}
      </AnimatePresence>
    </motion.button>
  )
}

// ─── Internal: StatusHero ────────────────────────────────────────────────────

function StatusHero({
  title,
  body,
}: {
  title: string
  body: ReactNode
}) {
  return (
    <div className="flex flex-col items-center gap-3.5 px-0 py-3 text-center">
      <span className="relative inline-flex size-[72px] items-center justify-center rounded-full bg-primary/10 text-primary">
        <ShieldCheck size={28} />
        <span className="absolute -inset-2 animate-ping rounded-full border border-current opacity-25" />
      </span>
      <p className="text-[11px] font-medium uppercase tracking-[0.14em] text-primary">
        Password updated
      </p>
      <h2 className="mt-1.5 font-serif text-[26px] font-medium leading-[1.15]">
        {title}
      </h2>
      <p className="max-w-[360px] text-sm leading-relaxed text-muted-foreground [text-wrap:pretty]">
        {body}
      </p>
    </div>
  )
}

// ─── Internal: ErrorBanner ───────────────────────────────────────────────────

function ErrorBanner({ message }: { message: string | null }) {
  return (
    <AnimatePresence>
      {message ? (
        <motion.div
          role="alert"
          initial={{ height: 0, marginBottom: 0, opacity: 0 }}
          animate={{ height: "auto", marginBottom: 14, opacity: 1 }}
          exit={{ height: 0, marginBottom: 0, opacity: 0 }}
          transition={{ duration: 0.38, ease: [0.16, 1, 0.3, 1] }}
          style={{ overflow: "hidden" }}
        >
          <div className="flex items-start gap-2.5 rounded-[10px] border border-destructive/[0.22] bg-destructive/[0.06] p-[10px_12px] text-[13px] leading-[1.45] text-destructive">
            <AlertCircle size={16} className="mt-px shrink-0" />
            <div>{message}</div>
          </div>
        </motion.div>
      ) : null}
    </AnimatePresence>
  )
}

// ─── Internal: TextLink ──────────────────────────────────────────────────────

function TextLink({
  children,
  onClick,
  className,
}: {
  children: ReactNode
  onClick: () => void
  className?: string
}) {
  return (
    <button
      type="button"
      className={cn(
        "cursor-pointer border-none bg-transparent p-0 text-[13px] text-muted-foreground hover:text-primary",
        className,
      )}
      onClick={onClick}
    >
      {children}
    </button>
  )
}

// ─── Internal: IdentityRow (register stage 2) ────────────────────────────────

function IdentityRow({
  username,
  onEdit,
}: {
  username: string
  onEdit: () => void
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-[10px] border border-border bg-muted px-3 py-2">
      <span className="flex min-w-0 items-center gap-2.5">
        <span className="inline-flex size-7 shrink-0 items-center justify-center rounded-full bg-primary/10 font-mono text-xs font-medium uppercase text-primary">
          {username.trim().charAt(0) || "@"}
        </span>
        <span className="truncate text-[13.5px] text-foreground">
          {username}
        </span>
      </span>
      <button
        type="button"
        className="inline-flex shrink-0 cursor-pointer items-center gap-1 border-none bg-transparent p-0 text-xs font-medium text-primary hover:text-primary/80"
        onClick={onEdit}
      >
        <Pencil size={12} /> Change
      </button>
    </div>
  )
}

// ─── Flow management hook ────────────────────────────────────────────────────

function useFlowManager(surgeClient: SurgeClient, open: boolean) {
  const [flowId, setFlowId] = useState<string | null>(null)
  const [csrfToken, setCsrfToken] = useState<string | null>(null)
  const [initError, setInitError] = useState<string | null>(null)

  const initFlow = useCallback(async () => {
    try {
      const flow = await surgeClient.initLoginFlow()
      setFlowId(flow.flow_id)
      setCsrfToken(flow.csrf_token)
      setInitError(null)
    } catch (err) {
      setInitError(getSurgeErrorMessage(err))
    }
  }, [surgeClient])

  useEffect(() => {
    if (!open) return
    setFlowId(null)
    setCsrfToken(null)
    setInitError(null)
    void initFlow()
  }, [open, initFlow])

  const reinitFlow = useCallback(async () => {
    try {
      const flow = await surgeClient.initLoginFlow()
      setFlowId(flow.flow_id)
      setCsrfToken(flow.csrf_token)
    } catch {
      // caller has already surfaced the error
    }
  }, [surgeClient])

  return { flowId, csrfToken, initError, reinitFlow }
}

// ─── View: Login ─────────────────────────────────────────────────────────────

function LoginView({
  surgeClient,
  flowId,
  csrfToken,
  reinitFlow,
  onAuthenticated,
  onNavigate,
  mark,
  wordlist,
}: {
  surgeClient: SurgeClient
  flowId: string | null
  csrfToken: string | null
  reinitFlow: () => Promise<void>
  onAuthenticated: (result: FlowResult) => void
  onNavigate: (view: AuthDialogView) => void
  mark?: ReactNode
  wordlist?: string[]
}) {
  const [username, setUsername] = useState("")
  const [password, setPassword] = useState("")
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [view, setView] = useState<LoginSubView>("form")
  const [totpCode, setTotpCode] = useState("")
  const [totpError, setTotpError] = useState(false)
  const [passphrase, setPassphrase] = useState("")
  const { stage, goToStage, getStageProps } = useAuthStageTransition()

  const canContinue = username.length >= 2
  const canSubmit = canContinue && password.length >= 6 && !loading
  const passphraseWordCount = passphrase.trim()
    ? passphrase.trim().split(/\s+/).length
    : 0
  const canPassphrase =
    canContinue && passphraseWordCount === PASSPHRASE_WORD_COUNT && !loading
  const stage1Props = getStageProps(1)
  const stage2Props = getStageProps(2)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (stage === 1) {
      if (canContinue) void goToStage(2)
      return
    }
    if (!canSubmit || !flowId || !csrfToken) return
    setLoading(true)
    setError(null)

    try {
      const result = await surgeClient.submitPassword(flowId, {
        username,
        password,
        csrf_token: csrfToken,
      })

      if ("status" in result && result.status === "totp_required") {
        setTotpCode("")
        setTotpError(false)
        setView("totp")
        return
      }

      onAuthenticated(result as FlowResult)
    } catch (err) {
      if (err instanceof SurgeError && err.code === "invalid_token") {
        await reinitFlow()
      }
      setError(getSurgeErrorMessage(err))
    } finally {
      setLoading(false)
    }
  }

  const handleTotpSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (totpCode.length !== 6 || !flowId || !csrfToken || loading) return
    setLoading(true)
    setError(null)

    try {
      const result = await surgeClient.submitTotp(flowId, {
        code: totpCode,
        csrf_token: csrfToken,
      })
      onAuthenticated(result)
    } catch (err) {
      setTotpError(true)
      setTotpCode("")
      if (err instanceof SurgeError && err.code === "invalid_token") {
        await reinitFlow()
        setView("form")
      }
      setError(getSurgeErrorMessage(err))
    } finally {
      setLoading(false)
    }
  }

  const handlePassphraseSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    const phrase = passphrase.trim()
    if (!phrase || !canContinue || !flowId || !csrfToken || loading) return
    setLoading(true)
    setError(null)

    try {
      const result = await surgeClient.submitPassphrase(flowId, {
        username,
        passphrase: phrase,
        csrf_token: csrfToken,
      })
      onAuthenticated(result)
    } catch (err) {
      if (err instanceof SurgeError && err.code === "invalid_token") {
        await reinitFlow()
      }
      setError(getSurgeErrorMessage(err))
    } finally {
      setLoading(false)
    }
  }

  return (
    <CrossFade viewKey={view}>
      {view === "form" ? (
        <>
          <ErrorBanner message={error} />
          <form onSubmit={handleSubmit} noValidate>
            <div className="relative">
              <motion.div {...stage1Props}>
                <div className="flex flex-col">
                  <AuthHeading
                    title="Sign in"
                    description="Enter your username to access your account."
                    mark={mark}
                  />
                  <div className="flex flex-col gap-3.5">
                    <Input
                      id="login-username"
                      name="username"
                      label="Username"
                      type="text"
                      icon={<User size={16} />}
                      required
                      value={username}
                      onChange={setUsername}
                      autoComplete="username"
                      placeholder="yourusername"
                      validate={(v) =>
                        !v || v.length >= 2 ? null : "At least 2 characters."
                      }
                    />
                  </div>
                  <div className="mt-[22px] flex flex-col items-center gap-3.5">
                    <Button type="submit" fullWidth disabled={!canContinue}>
                      Continue <ArrowRight size={16} />
                    </Button>
                    <p className="text-center text-[13px] text-muted-foreground">
                      New here?{" "}
                      <TextLink onClick={() => onNavigate("register")}>
                        Create an account
                      </TextLink>
                    </p>
                  </div>
                </div>
              </motion.div>

              <motion.div {...stage2Props}>
                <div className="flex flex-col">
                  <AuthHeading title="Welcome back" mark={mark} />
                  <div className="flex flex-col gap-3.5">
                    <IdentityBadge
                      username={username}
                      onEdit={() => void goToStage(1)}
                    />
                    <PasswordField
                      id="login-password"
                      name="password"
                      label="Password"
                      required
                      value={password}
                      onChange={setPassword}
                      autoComplete="current-password"
                      placeholder="Your password"
                      validate={(v) =>
                        !v || v.length >= 6 ? null : "Enter your password."
                      }
                      labelAction={
                        <TextLink onClick={() => onNavigate("reset-password")}>
                          Forgot?
                        </TextLink>
                      }
                    />
                  </div>
                  <div className="mt-[22px] flex flex-col items-center gap-3.5">
                    <Button
                      loading={loading}
                      type="submit"
                      fullWidth
                      disabled={!canSubmit}
                    >
                      Sign in <ArrowRight size={16} />
                    </Button>
                    {wordlist ? (
                      <TextLink
                        className="inline-flex items-center gap-1"
                        onClick={() => setView("options")}
                      >
                        <Ellipsis size={14} /> Use another option
                      </TextLink>
                    ) : null}
                    <TextLink onClick={() => void goToStage(1)}>Back</TextLink>
                  </div>
                </div>
              </motion.div>
            </div>
          </form>
        </>
      ) : view === "options" ? (
        <div className="flex flex-col">
          <AuthHeading
            title="Another way to sign in"
            description="Choose a different factor to finish signing in."
            mark={mark}
          />
          <div className="overflow-hidden rounded-xl border border-border">
            <button
              type="button"
              className="flex w-full items-center gap-3 bg-card p-3.5 text-left transition-colors hover:bg-muted"
              onClick={() => {
                setPassphrase("")
                setView("passphrase")
              }}
            >
              <span className="inline-flex size-10 shrink-0 items-center justify-center rounded-[10px] bg-primary/10 text-primary">
                <SquareAsterisk size={20} />
              </span>
              <span className="flex min-w-0 flex-col gap-0.5">
                <span className="text-[13.5px] font-medium text-foreground">
                  Use passphrase
                </span>
                <span className="text-xs text-muted-foreground">
                  Sign in with your 6-word passphrase
                </span>
              </span>
              <ChevronRight
                size={16}
                className="ml-auto shrink-0 text-muted-foreground"
              />
            </button>
          </div>
          <div className="mt-[22px] flex flex-col items-center gap-3.5">
            <TextLink
              className="inline-flex items-center gap-1"
              onClick={() => setView("form")}
            >
              <ArrowLeft size={14} /> Back
            </TextLink>
          </div>
        </div>
      ) : view === "totp" ? (
        <>
          <ErrorBanner message={error} />
          <form onSubmit={handleTotpSubmit} noValidate>
            <div className="flex flex-col">
              <AuthHeading
                title="Two-factor verification"
                description="Enter the 6-digit code from your TOTP app to finish signing in."
                mark={mark}
              />
              <div className="flex flex-col items-center gap-3.5">
                <span className="inline-flex size-11 items-center justify-center rounded-full bg-primary/10 text-primary">
                  <ShieldCheck size={22} />
                </span>
                <TotpInput
                  value={totpCode}
                  onChange={(v) => {
                    setTotpCode(v)
                    if (totpError) setTotpError(false)
                    if (error) setError(null)
                  }}
                  error={totpError}
                  autoFocus
                />
              </div>
              <div className="mt-[22px] flex flex-col items-center gap-3.5">
                <Button
                  loading={loading}
                  type="submit"
                  fullWidth
                  disabled={totpCode.length !== 6 || loading}
                >
                  Verify <ArrowRight size={16} />
                </Button>
                <TextLink
                  className="inline-flex items-center gap-1"
                  onClick={() => {
                    setTotpCode("")
                    setTotpError(false)
                    setError(null)
                    setView("form")
                  }}
                >
                  <ArrowLeft size={14} /> Back
                </TextLink>
              </div>
            </div>
          </form>
        </>
      ) : (
        <>
          <ErrorBanner message={error} />
          <form onSubmit={handlePassphraseSubmit} noValidate>
            <div className="flex flex-col">
              <AuthHeading
                title="Sign in with a passphrase"
                description="Enter your passphrase to sign in without your password or authenticator."
                mark={mark}
              />
              <div className="flex flex-col gap-3.5">
                <IdentityBadge
                  username={username}
                  onEdit={() => {
                    setPassphrase("")
                    void goToStage(1)
                    setView("form")
                  }}
                />
                <div className="flex flex-col items-center gap-2">
                  <label
                    htmlFor="login-passphrase"
                    className="text-[13px] text-muted-foreground"
                  >
                    Passphrase
                  </label>
                  <PassphraseInput
                    id="login-passphrase"
                    value={passphrase}
                    onChange={setPassphrase}
                    wordCount={PASSPHRASE_WORD_COUNT}
                    wordlist={wordlist!}
                    aria-label="Passphrase"
                  />
                </div>
              </div>
              <div className="mt-[22px] flex flex-col items-center gap-3.5">
                <Button
                  loading={loading}
                  type="submit"
                  fullWidth
                  disabled={!canPassphrase}
                >
                  Sign in <ArrowRight size={16} />
                </Button>
                <TextLink
                  className="inline-flex items-center gap-1"
                  onClick={() => {
                    setPassphrase("")
                    setError(null)
                    setView("options")
                  }}
                >
                  <ArrowLeft size={14} /> Back
                </TextLink>
              </div>
            </div>
          </form>
        </>
      )}
    </CrossFade>
  )
}

// ─── View: Register ──────────────────────────────────────────────────────────

function RegisterView({
  surgeClient,
  flowId,
  csrfToken,
  reinitFlow,
  onAuthenticated,
  onNavigate,
  mark,
}: {
  surgeClient: SurgeClient
  flowId: string | null
  csrfToken: string | null
  reinitFlow: () => Promise<void>
  onAuthenticated: (result: FlowResult) => void
  onNavigate: (view: AuthDialogView) => void
  mark?: ReactNode
}) {
  const [username, setUsername] = useState("")
  const [password, setPassword] = useState("")
  const [accepted, setAccepted] = useState(false)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const { stage, goToStage, getStageProps } = useAuthStageTransition()

  const score = pwScore(password)
  const canContinue = username.length >= 2
  const canSubmit = canContinue && password.length >= 8 && accepted && !loading
  const stage1Props = getStageProps(1)
  const stage2Props = getStageProps(2)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (stage === 1) {
      if (canContinue) void goToStage(2)
      return
    }
    if (!canSubmit || !flowId || !csrfToken) return
    setLoading(true)
    setError(null)

    try {
      const result = await surgeClient.register(flowId, {
        username,
        password,
        display_name: "",
        csrf_token: csrfToken,
      })
      onAuthenticated(result)
    } catch (err) {
      if (err instanceof SurgeError && err.code === "invalid_token") {
        await reinitFlow()
      }
      setError(getSurgeErrorMessage(err))
    } finally {
      setLoading(false)
    }
  }

  return (
    <>
      <ErrorBanner message={error} />
      <form onSubmit={handleSubmit} noValidate>
        <div className="relative">
          <motion.div {...stage1Props}>
            <div className="flex flex-col">
              <AuthHeading
                title="Create your account"
                description="One account for every service. Pick a username your team can find you with."
                mark={mark}
              />
              <div className="flex flex-col gap-3.5">
                <Input
                  id="register-username"
                  name="username"
                  label="Username"
                  type="text"
                  icon={<User size={16} />}
                  required
                  value={username}
                  onChange={setUsername}
                  autoComplete="username"
                  placeholder="yourusername"
                  validate={(v) =>
                    !v || v.length >= 2 ? null : "At least 2 characters."
                  }
                />
              </div>
              <div className="mt-[22px] flex flex-col items-center gap-3.5">
                <Button type="submit" fullWidth disabled={!canContinue}>
                  Continue <ArrowRight size={16} />
                </Button>
                <p className="text-center text-[13px] text-muted-foreground">
                  Have an account?{" "}
                  <TextLink onClick={() => onNavigate("login")}>
                    Sign in
                  </TextLink>
                </p>
              </div>
            </div>
          </motion.div>

          <motion.div {...stage2Props}>
            <div className="flex flex-col">
              <AuthHeading
                title="Almost there"
                description="Choose a password to finish setting up your account."
                mark={mark}
              />
              <div className="flex flex-col gap-3.5">
                <IdentityRow username={username} onEdit={() => void goToStage(1)} />
                <div className="flex flex-col">
                  <PasswordField
                    id="register-password"
                    name="password"
                    label="Password"
                    required
                    value={password}
                    onChange={setPassword}
                    autoComplete="new-password"
                    placeholder="At least 8 characters"
                    hint="Mix uppercase, numbers, and a symbol for a stronger password."
                    validate={(v) =>
                      !v || v.length >= 8 ? null : "At least 8 characters."
                    }
                  />
                  <AnimatePresence initial={false}>
                    {password ? (
                      <motion.div
                        key="pw-strength"
                        initial={{ opacity: 0, y: -6, height: 0 }}
                        animate={{ opacity: 1, y: 0, height: "auto" }}
                        exit={{ opacity: 0, y: -6, height: 0 }}
                        transition={{ duration: 0.18, ease: EASE }}
                        style={{ overflow: "hidden" }}
                      >
                        <StrengthMeter score={score} />
                      </motion.div>
                    ) : null}
                  </AnimatePresence>
                </div>
                <label className="mt-1 flex cursor-pointer items-start gap-2">
                  <input
                    type="checkbox"
                    checked={accepted}
                    onChange={(e) => setAccepted(e.target.checked)}
                    className="mt-0.5 size-4 shrink-0 rounded border border-border accent-primary"
                  />
                  <span className="text-[13px] leading-[1.55] text-muted-foreground">
                    I agree to the Terms and Privacy Policy.
                  </span>
                </label>
              </div>
              <div className="mt-[22px] flex flex-col items-center gap-3.5">
                <Button
                  loading={loading}
                  type="submit"
                  fullWidth
                  disabled={!canSubmit}
                >
                  Create account <ArrowRight size={16} />
                </Button>
                <TextLink onClick={() => void goToStage(1)}>Back</TextLink>
              </div>
            </div>
          </motion.div>
        </div>
      </form>
    </>
  )
}

// ─── View: Reset Password ────────────────────────────────────────────────────

function ResetPasswordView({
  surgeClient,
  flowId,
  csrfToken,
  reinitFlow,
  onAuthenticated,
  onNavigate,
  mark,
}: {
  surgeClient: SurgeClient
  flowId: string | null
  csrfToken: string | null
  reinitFlow: () => Promise<void>
  onAuthenticated: (result: FlowResult) => void
  onNavigate: (view: AuthDialogView) => void
  mark?: ReactNode
}) {
  const [username, setUsername] = useState("")
  const [passphrase, setPassphrase] = useState("")
  const [password, setPassword] = useState("")
  const [confirm, setConfirm] = useState("")
  const [loading, setLoading] = useState(false)
  const [doneResult, setDoneResult] = useState<FlowResult | null>(null)
  const [error, setError] = useState<string | null>(null)

  const score = pwScore(password)
  const canSubmit =
    username.length >= 2 &&
    passphrase.trim().length > 0 &&
    password.length >= 8 &&
    password === confirm &&
    !loading

  const submit = async (event?: React.FormEvent) => {
    event?.preventDefault()
    if (!canSubmit || !flowId || !csrfToken) return
    setLoading(true)
    setError(null)

    try {
      const result = await surgeClient.recoverPassword(flowId, {
        username,
        passphrase: passphrase.trim(),
        new_password: password,
        csrf_token: csrfToken,
      })
      setDoneResult(result)
    } catch (err) {
      if (err instanceof SurgeError && err.code === "invalid_token") {
        await reinitFlow()
      }
      setError(getSurgeErrorMessage(err))
    } finally {
      setLoading(false)
    }
  }

  if (doneResult) {
    return (
      <>
        <StatusHero
          title="New password set."
          body="Your passphrase checked out and you're signed in. For your security, every other session has been signed out."
        />
        <div className="mt-[22px] flex flex-col items-center gap-3.5">
          <Button onClick={() => onAuthenticated(doneResult)} fullWidth>
            Continue <ArrowRight size={16} />
          </Button>
        </div>
      </>
    )
  }

  return (
    <>
      <ErrorBanner message={error} />
      <AuthHeading
        title="Reset with your passphrase"
        description="Enter your recovery passphrase to set a new password."
        mark={mark}
      />

      <form className="flex flex-col gap-3.5" onSubmit={submit} noValidate>
        <Input
          id="reset-username"
          name="username"
          label="Username"
          type="text"
          icon={<User size={16} />}
          required
          value={username}
          onChange={setUsername}
          autoComplete="username"
          placeholder="yourusername"
          validate={(v) => (!v || v.length >= 2 ? null : "At least 2 characters.")}
        />
        <PasswordField
          id="reset-passphrase"
          name="passphrase"
          label="Recovery passphrase"
          required
          value={passphrase}
          onChange={setPassphrase}
          autoComplete="off"
          placeholder="Your recovery passphrase"
          capsHint={false}
        />
        <PasswordField
          id="reset-password"
          label="New password"
          required
          value={password}
          onChange={setPassword}
          autoComplete="new-password"
          placeholder="At least 8 characters"
          validate={(v) =>
            !v || v.length >= 8 ? null : "At least 8 characters."
          }
        />
        {password ? <StrengthMeter score={score} /> : null}
        <PasswordField
          id="reset-password-confirm"
          label="Confirm new password"
          required
          value={confirm}
          onChange={setConfirm}
          autoComplete="new-password"
          placeholder="Re-enter the password"
          validate={(v) =>
            !v || v === password ? null : "Passwords do not match."
          }
        />

        <div className="mt-[22px] flex flex-col items-center gap-3.5">
          <Button loading={loading} type="submit" fullWidth disabled={!canSubmit}>
            Update password <ArrowRight size={16} />
          </Button>
          <TextLink onClick={() => onNavigate("login")}>
            Back to sign in
          </TextLink>
        </div>
      </form>
    </>
  )
}

// ─── Main: AuthDialog ────────────────────────────────────────────────────────

export function AuthDialog({
  surgeClient,
  open,
  onOpenChange,
  onAuthenticated,
  defaultView = "login",
  mark,
  wordlist,
  dismissible = false,
}: AuthDialogProps) {
  const [currentView, setCurrentView] = useState<AuthDialogView>(defaultView)
  const { flowId, csrfToken, initError, reinitFlow } = useFlowManager(
    surgeClient,
    open,
  )

  useEffect(() => {
    if (open) setCurrentView(defaultView)
  }, [open, defaultView])

  const handleOpenChange = (next: boolean) => {
    if (!next && !dismissible) return
    onOpenChange?.(next)
  }

  const handleNavigate = useCallback(async (view: AuthDialogView) => {
    setCurrentView(view)
    await reinitFlow()
  }, [reinitFlow])

  const viewProps = {
    surgeClient,
    flowId,
    csrfToken,
    reinitFlow,
    onAuthenticated,
    onNavigate: handleNavigate,
    mark,
    wordlist,
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      {/*
        `p-0` hands the dialog's inset to the padded div below, inside the
        height animation's clip box — see `HeightAnimationWrapper`. The
        breakpoint here matches `useIsMobile`'s 640px, so the padding tracks
        which surface (dialog or drawer) is actually rendering.
      */}
      <DialogContent
        showCloseButton={dismissible}
        className="p-0 sm:max-w-[440px]"
        onEscapeKeyDown={dismissible ? undefined : (e) => e.preventDefault()}
        onPointerDownOutside={
          dismissible ? undefined : (e) => e.preventDefault()
        }
        onInteractOutside={
          dismissible ? undefined : (e) => e.preventDefault()
        }
      >
        <DialogTitle className="sr-only">Authentication</DialogTitle>
        <HeightAnimationWrapper>
          <div className="px-6 pb-6 pt-4 sm:pt-6">
            <ErrorBanner message={initError} />
            <CrossFade viewKey={currentView}>
              {currentView === "login" ? (
                <LoginView {...viewProps} />
              ) : currentView === "register" ? (
                <RegisterView {...viewProps} />
              ) : (
                <ResetPasswordView {...viewProps} />
              )}
            </CrossFade>
          </div>
        </HeightAnimationWrapper>
      </DialogContent>
    </Dialog>
  )
}
