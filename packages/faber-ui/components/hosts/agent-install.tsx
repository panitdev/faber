"use client"

import * as React from "react"
import { Check, Copy } from "lucide-react"

import { FaberError, type AgentEnrollment, type AgentStatus } from "@/lib/api"
import { Button } from "@/components/ui/button"
import { DialogFooter } from "@/components/ui/responsive-dialog"
import { FlowDialogHeading } from "@/components/ui/flow-dialog"

/** How often the install view asks whether the daemon has shown up. */
const AGENT_POLL_MS = 2000

export function CommandBlock({ command }: { command: string }) {
  const [copied, setCopied] = React.useState(false)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    } catch {
      // Clipboard access can be refused. The command is selectable text, so
      // there is still a way to take it — nothing to report.
    }
  }

  return (
    <div className="flex items-start gap-2 rounded-lg border border-border bg-muted/40 p-2.5">
      {/* Wrapped, not scrolled: a command that runs off the side hides the
          half that carries the token behind a scrollbar sitting on top of the
          text. It is meant to be read and copied, and three lines cost less
          than either. */}
      <code className="min-w-0 flex-1 break-all whitespace-pre-wrap text-xs leading-relaxed">
        {command}
      </code>
      <Button
        type="button"
        size="icon-sm"
        variant="ghost"
        className="shrink-0"
        aria-label="Copy the install command"
        onClick={() => void copy()}
      >
        {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
      </Button>
    </div>
  )
}

/**
 * The waiting half of the flow: hand over a command, then watch for the
 * daemon it installs to dial in.
 *
 * Everything here is scoped to one bootstrap token, which is shown once and
 * stored nowhere — a token issued and then lost is replaced by issuing
 * another, never by reading the old one back.
 *
 * The two calls arrive as props rather than being made here so the host
 * dialog and the install re-entry share one view: a token shown once, an
 * hour to use it, a poll that stops the moment the daemon appears.
 */
export function AgentInstall({
  hostName,
  description,
  open,
  reentry,
  issue: issueCommand,
  status: readStatus,
  onConnected,
}: {
  hostName: string
  /** One line under the title, saying what the command will do to the
   *  machine. The two callers install daemons with very different authority,
   *  and the person about to paste it is owed which one this is. */
  description: string
  open: boolean
  /**
   * Whether this view was reached by asking for the install command again
   * rather than by creating the host.
   *
   * It decides one thing, and it matters: issuing a command *supersedes* the
   * previous one. Doing that on arrival would kill the command the user is
   * halfway through running on the machine, and the failure would surface
   * over there rather than here. So a re-entry waits, and issues only when
   * the user asks for it.
   */
  reentry: boolean
  issue: () => Promise<AgentEnrollment>
  status: () => Promise<AgentStatus>
  onConnected: () => void
}) {
  const [enrollment, setEnrollment] = React.useState<AgentEnrollment | null>(null)
  const [error, setError] = React.useState<string | null>(null)
  const [issuing, setIssuing] = React.useState(false)
  const [enrolled, setEnrolled] = React.useState(false)
  const [expired, setExpired] = React.useState(false)

  const issue = React.useCallback(async () => {
    setIssuing(true)
    setError(null)
    try {
      setEnrollment(await issueCommand())
      setExpired(false)
    } catch (err) {
      // The server's message names what to fix — an unset public URL, most
      // often — and replacing it with a generic failure throws that away.
      setError(
        err instanceof FaberError ? err.message : "failed to issue an install command",
      )
    } finally {
      setIssuing(false)
    }
  }, [issueCommand])

  React.useEffect(() => {
    if (reentry) return
    void issue()
  }, [reentry, issue])

  React.useEffect(() => {
    if (!open || expired) return

    let cancelled = false
    let inFlight = false

    const tick = async () => {
      if (inFlight) return
      // A token nobody redeemed stops being redeemable, so polling past that
      // point would ask a question whose answer can no longer change. An
      // unparseable timestamp is left alone rather than treated as expired:
      // the dialog closing ends the poll either way.
      const deadline = enrollment ? Date.parse(enrollment.expires_at) : NaN
      if (!Number.isNaN(deadline) && Date.now() > deadline) {
        if (!cancelled) setExpired(true)
        return
      }
      inFlight = true
      try {
        const status = await readStatus()
        if (cancelled) return
        setEnrolled(!!status.enrolled_at)
        if (status.connected) onConnected()
      } catch {
        // A failed poll says nothing about the daemon; the next one asks
        // again.
      } finally {
        inFlight = false
      }
    }

    void tick()
    const timer = window.setInterval(() => void tick(), AGENT_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [open, enrollment, expired, readStatus, onConnected])

  const waiting = (
    <div className="flex items-center gap-2 text-sm text-muted-foreground">
      <span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-muted-foreground" />
      {enrolled
        ? "The daemon enrolled. Waiting for it to connect…"
        : "Waiting for the daemon to connect…"}
    </div>
  )

  const reissue = (label: string) => (
    <DialogFooter>
      <Button type="button" onClick={() => void issue()} loading={issuing} loadingText="Issuing">
        {label}
      </Button>
    </DialogFooter>
  )

  return (
    <div className="flex flex-col gap-4">
      <FlowDialogHeading
        title={`Install the daemon on ${hostName}`}
        description={description}
      />

      {error ? (
        <>
          <p className="text-sm text-destructive">{error}</p>
          <DialogFooter>
            <Button type="button" onClick={() => void issue()} loading={issuing} loadingText="Retrying">
              Try again
            </Button>
          </DialogFooter>
        </>
      ) : enrollment ? (
        <>
          <CommandBlock command={enrollment.install_command} />

          <p className="text-xs text-muted-foreground">
            The token in it works once, for an hour. Anyone who can read the
            command can enrol this host, so treat it as the secret it is.
          </p>

          {expired ? (
            <>
              <p className="text-sm text-muted-foreground">
                This command has expired. Issue another to keep going.
              </p>
              {reissue("New command")}
            </>
          ) : (
            waiting
          )}

          <p className="text-xs text-muted-foreground">
            You can close this. The host is already saved, and it becomes usable
            the moment the daemon connects.
          </p>
        </>
      ) : reentry ? (
        <>
          {waiting}
          <p className="text-xs text-muted-foreground">
            The command issued earlier is shown once and kept nowhere, so there
            is nothing to show again. Issuing a new one is the way back — and it
            retires the old command, which stops working the moment this does.
          </p>
          {reissue("New command")}
        </>
      ) : (
        <p className="text-sm text-muted-foreground">Issuing an install command…</p>
      )}
    </div>
  )
}

export function AgentConnected({
  hostName,
  onDone,
}: {
  hostName: string
  onDone: () => void
}) {
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col items-center gap-3 py-2 text-center">
        <span className="flex h-10 w-10 items-center justify-center rounded-full bg-muted">
          <Check className="h-5 w-5" />
        </span>
        <FlowDialogHeading
          title={`${hostName} is connected`}
          description="The daemon dialed in and faber is holding the connection. Nothing else to install."
        />
      </div>

      <p className="rounded-lg border border-border bg-muted/40 px-3 py-2.5 text-xs text-muted-foreground">
        If the machine reboots or the network drops, the daemon reconnects on
        its own — faber never dials this host, so there is nothing to re-run
        here.
      </p>

      <DialogFooter>
        <Button type="button" onClick={onDone}>
          Done
        </Button>
      </DialogFooter>
    </div>
  )
}
