import { cn } from "@/lib/utils"
import type { ReactNode } from "react"

const TONE = {
  ok: "bg-success",
  warn: "bg-warn",
  bad: "bg-alert",
  idle: "bg-strong",
  info: "bg-ok",
} as const
export type Tone = keyof typeof TONE

/** What a dot means, for screen readers when there's no text next to it. */
const SAID: Record<Tone, string> = { ok: "ok", warn: "warning", bad: "problem", idle: "inactive", info: "in progress" }

/** A status dot. Decorative next to text; `label` (or the tone) is read out when it stands alone. */
export function Dot({ tone, pulse, label }: { tone: Tone; pulse?: boolean; label?: string | false }) {
  return (
    <span className="relative inline-flex size-2 shrink-0">
      {pulse && <span aria-hidden className={cn("absolute inline-flex size-full animate-ping rounded-full opacity-60", TONE[tone])} />}
      <span aria-hidden className={cn("relative inline-flex size-2 rounded-full", TONE[tone])} />
      {label !== false && <span className="sr-only">{label ?? SAID[tone]}</span>}
    </span>
  )
}

export function State({ tone, children, pulse }: { tone: Tone; children: ReactNode; pulse?: boolean }) {
  const empty = children == null || children === ""
  return (
    <span className="inline-flex items-center gap-1.5 text-xs whitespace-nowrap">
      <Dot tone={tone} pulse={pulse} label={empty ? undefined : false} />
      {children}
    </span>
  )
}

/** A thin usage bar; turns amber past 75%, red past 90%. `null` (unknown) is a dashed empty bar. */
export function Meter({ value, className }: { value: number | null | undefined; className?: string }) {
  if (value == null || !Number.isFinite(value))
    return <div title="no reading" className={cn("h-1.5 w-full rounded-full border border-dashed border-border", className)} />
  const v = Math.max(0, Math.min(1, value))
  return (
    <div className={cn("h-1.5 w-full overflow-hidden rounded-full bg-accent", className)}>
      <div
        className={cn("h-full rounded-full", v > 0.9 ? "bg-alert" : v > 0.75 ? "bg-warn" : "bg-foreground")}
        style={{ width: `${v * 100}%` }}
      />
    </div>
  )
}

/** The one empty state, as in dux: a dashed box, centred. */
export function Empty({ children }: { children: ReactNode }) {
  return <div className="m-3 rounded-lg border border-dashed px-6 py-10 text-center text-sm text-muted-foreground">{children}</div>
}

export function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("font-mono text-xs", className)}>{children}</span>
}

export function containerTone(state: string): Tone {
  if (state === "running") return "ok"
  if (state === "restarting" || state === "paused") return "warn"
  if (state === "dead") return "bad"
  return "idle"
}

export function jobTone(state: string, exit: number | null): Tone {
  if (state === "running") return "info"
  if (state === "cancelled") return "idle"
  if (state === "failed" || state === "lost" || (exit != null && exit !== 0)) return "bad"
  return "ok"
}

export function releaseTone(state: string): Tone {
  if (state === "live") return "ok"
  if (state === "failed" || state === "lost") return "bad"
  if (state === "building" || state === "ready") return "info"
  if (state === "draining") return "warn"
  return "idle"
}
