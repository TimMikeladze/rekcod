"use client"

import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react"
import { ArrowDown, ArrowUp, Check, Copy, Download, Filter, Pause, Play, Regex, TriangleAlert, WrapText } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import type { Action } from "@/lib/types"
import { cn } from "@/lib/utils"

/** `since` values `/api/logs` accepts; "" is the last 1000 lines. */
export const SINCE = ["", "1m", "10m", "1h", "6h", "24h"] as const
export type Since = (typeof SINCE)[number]

/** What to stream: a container's logs (with flags) or an offered STREAMED action's output. */
type LogSource = { container: string } | { argv: string[] }

/** Keep the last ~400 KB so a chatty container can't eat the tab. */
const KEEP = 400_000
/** A line's height without wrap (`leading-5`), and the guess for wrapped lines not measured yet. */
const LINE_H = 20
/** Pixels rendered above and below the view. */
const OVERSCAN_PX = 600

/** Each row's top from its height (`heightOf(pos)`), plus the total height. */
export function layout(count: number, heightOf: (pos: number) => number): { tops: Float64Array; total: number } {
  const tops = new Float64Array(count)
  let y = 0
  for (let pos = 0; pos < count; pos++) {
    tops[pos] = y
    y += heightOf(pos)
  }
  return { tops, total: y }
}

/** The rows `[first, last)` that overlap `[top - pad, top + h + pad]`. */
export function windowOf(tops: Float64Array, total: number, top: number, h: number, pad = OVERSCAN_PX): [number, number] {
  const n = tops.length
  const find = (y: number) => {
    let lo = 0
    let hi = n
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (tops[mid] <= y) lo = mid + 1
      else hi = mid
    }
    return lo
  }
  if (!n) return [0, 0]
  const first = Math.max(0, find(Math.max(0, top - pad)) - 1)
  const last = Math.min(n, find(Math.min(total, top + h + pad)))
  return [first, Math.max(first + 1, last)]
}

function url(src: LogSource, since: Since, timestamps: boolean): string {
  if ("container" in src) {
    const p = new URLSearchParams({ name: src.container, follow: "1" })
    if (since) p.set("since", since)
    if (timestamps) p.set("timestamps", "1")
    return `/api/logs?${p}`
  }
  return `/api/stream?argv=${encodeURIComponent(JSON.stringify(src.argv))}`
}

export type Level = "error" | "warn" | null

const ERROR = /\b(error|err|fatal|panic|exception|failed|crit(ical)?)\b|\blevel=(error|fatal)\b/i
const WARN = /\b(warn|warning)\b|\blevel=warn/i

/** How bad a log line looks: error words win over warn words. */
export function lineLevel(line: string): Level {
  return ERROR.test(line) ? "error" : WARN.test(line) ? "warn" : null
}

/** The search as a global RegExp: `q` literally, or as a regex. A bad regex is an error, not a throw. */
export function compileSearch(q: string, regex: boolean): RegExp | { error: string } | null {
  if (!q) return null
  try {
    return new RegExp(regex ? q : q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi")
  } catch (e) {
    return { error: (e as Error).message.replace(/^Invalid regular expression: /, "") }
  }
}

/** Matches of `re` in `line`, skipping empty ones (`a*` would otherwise match everywhere). */
function matchesIn(line: string, re: RegExp): RegExpExecArray[] {
  re.lastIndex = 0
  return [...line.matchAll(re)].filter((m) => m[0].length > 0)
}

const LEVEL_CLASS = { error: "text-alert-text", warn: "text-warn-text" } as const

const Line = memo(function Line({ n, text, re, current }: { n: number; text: string; re: RegExp | null; current: boolean }) {
  const level = lineLevel(text)
  let body: React.ReactNode = text || " "
  if (re) {
    const out: React.ReactNode[] = []
    let last = 0
    for (const [i, m] of matchesIn(text, re).entries()) {
      out.push(text.slice(last, m.index))
      out.push(
        <mark key={i} className="rounded-sm bg-warn-border text-foreground">
          {m[0]}
        </mark>,
      )
      last = m.index + m[0].length
    }
    if (out.length) {
      out.push(text.slice(last))
      body = out
    }
  }
  return (
    <div data-line={n} className={cn("flex min-h-5 gap-3 px-3", current && "bg-ok-bg", level && LEVEL_CLASS[level])}>
      <span aria-hidden className="w-10 flex-none text-right text-muted-foreground/60 select-none tabular-nums">
        {n + 1}
      </span>
      <span className="min-w-0 flex-1">{body}</span>
    </div>
  )
})

/**
 * Streams a container's logs (`container`, through `/api/logs`) or a streamed action's output
 * (`action`, through `/api/stream`) into a scrolling panel: line numbers, warn/error colours,
 * search (plain or regex) with match count and next/previous, "Next error", "Only matching",
 * copy the lines shown, pause, wrap and download. Containers also get timestamps and a "since"
 * picker; changing those changes the `rekcod logs` flags, so it reconnects.
 *
 * Only the lines near the view are rendered, wrapped or not: wrapped lines are measured as
 * they appear and the rest are guessed at one line tall.
 */
export function LogView({ action, container, height = "70vh", className }: { action?: Action | null; container?: string; height?: string; className?: string }) {
  const name = container ?? (action?.kind === "ContainerLogs" ? action.argv?.at(-1) : undefined)
  const source: LogSource | null = name ? { container: name } : action?.argv ? { argv: action.argv } : null
  const fileName = `${(name ?? action?.argv?.join("_") ?? "output").replace(/[^\w.-]+/g, "_")}.log`
  const [text, setText] = useState("")
  // Paused: the text as it was then; new output keeps arriving into `text`.
  const [frozen, setFrozen] = useState<string | null>(null)
  const paused = frozen != null
  const shown = frozen ?? text
  const [wrap, setWrap] = useState(true)
  const [q, setQ] = useState("")
  const [regex, setRegex] = useState(false)
  const [only, setOnly] = useState(false)
  const [cur, setCur] = useState(-1)
  // The error line "Next error" last went to; highlighted like the current match.
  const [errLine, setErrLine] = useState(-1)
  const [since, setSince] = useState<Since>("")
  const [timestamps, setTimestamps] = useState(false)
  const [done, setDone] = useState(false)
  const [copied, setCopied] = useState(false)
  const [scroll, setScroll] = useState({ top: 0, h: 600, w: 0 })
  // Measured heights of wrapped lines, by line index; bumping `measured` re-lays out.
  const heights = useRef(new Map<number, number>())
  const [measured, setMeasured] = useState(0)
  const pendingMeasure = useRef(0)
  const box = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  const key = JSON.stringify(source)
  const isContainer = !!source && "container" in source

  useEffect(() => {
    const src = JSON.parse(key) as LogSource | null
    if (!src) return
    const ctl = new AbortController()
    follow.current = true
    ;(async () => {
      setText("")
      setDone(false)
      try {
        const r = await fetch(url(src, since, timestamps), { signal: ctl.signal })
        if (!r.ok || !r.body) {
          setText(await r.text())
          return
        }
        const reader = r.body.getReader()
        const dec = new TextDecoder()
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          setText((t) => {
            const next = t + dec.decode(value, { stream: true })
            // Cut at a line start so numbering doesn't begin mid-line.
            if (next.length <= KEEP) return next
            const cut = next.indexOf("\n", next.length - KEEP)
            return cut < 0 ? next.slice(-KEEP) : next.slice(cut + 1)
          })
        }
      } catch {}
      if (!ctl.signal.aborted) setDone(true)
    })()
    return () => ctl.abort()
  }, [key, since, timestamps])

  const lines = useMemo(() => {
    const l = shown.split("\n")
    if (l.at(-1) === "") l.pop()
    return l
  }, [shown])
  const search = useMemo(() => compileSearch(q, regex), [q, regex])
  const re = search instanceof RegExp ? search : null
  const hits = useMemo(() => (re ? lines.flatMap((l, i) => (matchesIn(l, re).length ? [i] : [])) : []), [lines, re])
  const matchCount = useMemo(() => (re ? hits.reduce((n, i) => n + matchesIn(lines[i], re).length, 0) : 0), [hits, lines, re])
  const errors = useMemo(() => lines.flatMap((l, i) => (lineLevel(l) === "error" ? [i] : [])), [lines])
  // Line indices on screen: every line, or only the matching ones.
  const visible = useMemo(() => (only && re ? hits : null), [only, re, hits])
  const count = visible ? visible.length : lines.length

  useLayoutEffect(() => {
    const el = box.current
    if (!el) return
    const ro = new ResizeObserver(() => setScroll({ top: el.scrollTop, h: el.clientHeight, w: el.clientWidth }))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // Wrapped heights depend on the width; a new stream renumbers the lines.
  useLayoutEffect(() => {
    heights.current.clear()
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setMeasured((m) => m + 1)
  }, [wrap, scroll.w, key, since, timestamps])

  const at = (pos: number) => (visible ? visible[pos] : pos)
  const { tops, total } = useMemo(
    () => layout(count, (pos) => (wrap ? (heights.current.get(visible ? visible[pos] : pos) ?? LINE_H) : LINE_H)),
    // `measured` stands for the heights map changing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [count, visible, wrap, measured],
  )

  /** A rendered wrapped line reports its height; changes are applied once per frame. */
  const measure = (i: number) => (el: HTMLDivElement | null) => {
    if (!el || !wrap) return
    const h = el.offsetHeight
    if (h && heights.current.get(i) !== h) {
      heights.current.set(i, h)
      if (!pendingMeasure.current)
        pendingMeasure.current = requestAnimationFrame(() => {
          pendingMeasure.current = 0
          setMeasured((m) => m + 1)
        })
    }
  }
  useEffect(() => () => cancelAnimationFrame(pendingMeasure.current), [])

  useLayoutEffect(() => {
    const el = box.current
    if (!paused && follow.current && el) el.scrollTop = el.scrollHeight
  }, [count, total, paused, wrap])

  /** Scrolls line `i` (an index into `lines`) to the middle of the panel. */
  function reveal(i: number) {
    const el = box.current
    if (!el) return
    follow.current = false
    const pos = visible ? visible.indexOf(i) : i
    if (pos >= 0) el.scrollTop = Math.max(0, tops[pos] - el.clientHeight / 2)
  }

  function jump(by: 1 | -1) {
    if (!hits.length) return
    const i = cur < 0 ? (by > 0 ? 0 : hits.length - 1) : (cur + by + hits.length) % hits.length
    setCur(i)
    reveal(hits[i])
  }

  /** The first error line after the one last jumped to (or after the top of the view). */
  function nextError() {
    if (!errors.length) return
    const el = box.current
    const fromLine = cur >= 0 && hits[cur] != null ? hits[cur] : el && count ? at(windowOf(tops, total, el.scrollTop, 0, 0)[0]) : 0
    const next = errors.find((i) => i > fromLine) ?? errors[0]
    if (visible && !visible.includes(next)) setOnly(false)
    setCur(-1)
    setErrLine(next)
    requestAnimationFrame(() => reveal(next))
  }
  async function copyShown() {
    const out = visible ? visible.map((i) => lines[i]).join("\n") : shown
    try {
      await navigator.clipboard.writeText(out)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {}
  }

  function download() {
    const a = document.createElement("a")
    a.href = URL.createObjectURL(new Blob([text], { type: "text/plain" }))
    a.download = fileName
    a.click()
    setTimeout(() => URL.revokeObjectURL(a.href), 1000)
  }

  const currentLine = cur >= 0 ? hits[cur] : errLine
  let rows: React.ReactNode
  if (!count) {
    rows = <div className="px-4 py-3 text-muted-foreground">{lines.length ? "No line matches." : done ? "no output" : "waiting for output…"}</div>
  } else {
    // Only the rows near the view exist; wrapped ones are measured as they render.
    const [first, last] = windowOf(tops, total, scroll.top, scroll.h)
    rows = (
      // Rows stay in normal flow after a spacer, so without wrap the longest one sets the width.
      <div style={{ boxSizing: "border-box", height: total, paddingTop: tops[first], minWidth: wrap ? undefined : "max-content" }}>
        {Array.from({ length: last - first }, (_, k) => {
          const pos = first + k
          const i = at(pos)
          return (
            <div key={i} ref={measure(i)}>
              <Line n={i} text={lines[i]} re={re} current={i === currentLine} />
            </div>
          )
        })}
      </div>
    )
  }

  const bad = search && !(search instanceof RegExp) ? search.error : null
  return (
    <div className={cn("flex min-h-0 flex-col overflow-hidden rounded-md border", className)} style={{ height }}>
      <div className="flex flex-wrap items-center gap-1.5 border-b px-3 py-2">
        <Button size="xs" variant="outline" onClick={() => setFrozen(paused ? null : text)} aria-pressed={paused} title={paused ? "Resume following" : "Pause"}>
          {paused ? <Play /> : <Pause />}
          {paused ? `Resume${text.length > shown.length ? " (new lines)" : ""}` : "Pause"}
        </Button>
        <Button size="xs" variant={wrap ? "secondary" : "outline"} onClick={() => setWrap((w) => !w)} aria-pressed={wrap}>
          <WrapText />
          Wrap
        </Button>
        {isContainer && (
          <>
            <Button size="xs" variant={timestamps ? "secondary" : "outline"} onClick={() => setTimestamps((t) => !t)} aria-pressed={timestamps}>
              Timestamps
            </Button>
            <label className="flex items-center gap-1 text-xs text-muted-foreground">
              Since
              <select
                value={since}
                onChange={(e) => setSince(e.target.value as Since)}
                className="h-6 rounded-md border bg-transparent px-1 font-mono text-xs text-foreground"
              >
                {SINCE.map((s) => (
                  <option key={s} value={s}>
                    {s || "last 1000 lines"}
                  </option>
                ))}
              </select>
            </label>
          </>
        )}
        <Button size="xs" variant="outline" onClick={nextError} disabled={!errors.length} title="Jump to the next error line">
          <TriangleAlert />
          Next error{errors.length ? <span className="font-mono text-muted-foreground tabular-nums">{errors.length}</span> : null}
        </Button>
        <div className="ml-auto flex flex-wrap items-center gap-1.5">
          <div className="relative">
            <Input
              value={q}
              onChange={(e) => {
                setQ(e.target.value)
                setCur(-1)
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault()
                  jump(e.shiftKey ? -1 : 1)
                }
              }}
              placeholder={regex ? "Regex" : "Search"}
              aria-label="Search the log"
              aria-invalid={!!bad}
              className={cn("h-6 w-40 pr-7 text-xs sm:w-48", regex && "font-mono")}
            />
            <button
              type="button"
              onClick={() => setRegex((r) => !r)}
              aria-pressed={regex}
              aria-label="Regular expression"
              title="Regular expression"
              className={cn("absolute top-1/2 right-1 inline-flex size-5 -translate-y-1/2 cursor-pointer items-center justify-center rounded text-muted-foreground hover:text-foreground", regex && "bg-accent text-foreground")}
            >
              <Regex className="size-3.5" />
            </button>
          </div>
          {q && (
            <span className={cn("font-mono text-[11px] tabular-nums", bad ? "text-alert-text" : "text-muted-foreground")} aria-live="polite">
              {bad ? "bad regex" : `${cur >= 0 ? `${cur + 1}/` : ""}${matchCount} match${matchCount === 1 ? "" : "es"}`}
            </span>
          )}
          {re && (
            <>
              <Button size="icon-xs" variant="ghost" onClick={() => jump(-1)} disabled={!hits.length} aria-label="Previous match" title="Previous match (Shift+Enter)">
                <ArrowUp />
              </Button>
              <Button size="icon-xs" variant="ghost" onClick={() => jump(1)} disabled={!hits.length} aria-label="Next match" title="Next match (Enter)">
                <ArrowDown />
              </Button>
              <Button size="xs" variant={only ? "secondary" : "ghost"} onClick={() => setOnly((o) => !o)} aria-pressed={only} title="Show only matching lines">
                <Filter />
                Only matching
              </Button>
            </>
          )}
          <Button size="icon-xs" variant="ghost" onClick={copyShown} aria-label="Copy the lines shown" title="Copy the lines shown">
            {copied ? <Check /> : <Copy />}
          </Button>
          <Button size="icon-xs" variant="ghost" onClick={download} aria-label="Download the log" title="Download">
            <Download />
          </Button>
        </div>
      </div>
      <div
        ref={box}
        tabIndex={0}
        role="log"
        aria-label="Log output"
        onScroll={(e) => {
          const el = e.currentTarget
          follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
          setScroll({ top: el.scrollTop, h: el.clientHeight, w: el.clientWidth })
        }}
        className={cn("min-h-0 flex-1 overflow-auto bg-subtle py-2 font-mono text-xs leading-5", wrap ? "whitespace-pre-wrap break-all" : "whitespace-pre")}
      >
        {rows}
      </div>
    </div>
  )
}
