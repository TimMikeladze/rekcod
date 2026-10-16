import "server-only"
import type { ChildProcess } from "node:child_process"
import type { State } from "@/lib/types"
import { FIXTURE, readState, spawnRekcod } from "@/lib/rekcod"
import { broadcast, once, subscribers } from "./bus"
import { mergeLocal } from "./merge"
import { fakeNodes, recordNodes } from "./nodes"
import { splitLines } from "./stats"

// The state the page sees. Docker events refresh `rekcod state --local` (no controller calls,
// well under a second) and push it; a full `rekcod state` runs every 15 s for the fleet parts.
// Polling clients (`GET /api/state`) get the cache when it's under 3 s old.
const FULL_MS = 15_000
const POLL_FRESH_MS = 3000
const DEBOUNCE_MS = 150
const IDLE_MS = 60_000
const FS_KEEP_MS = 3600_000
const WATCH = new Set(["container", "image", "volume", "network"])

type H = {
  current: State | null
  full: State | null
  fullAt: number
  at: number
  error: string | null
  inflight: Promise<State> | null
  fullInflight: Promise<State> | null
  events: ChildProcess | null
  backoff: number
  debounce: ReturnType<typeof setTimeout> | null
  timer: ReturnType<typeof setInterval> | null
  lastAsk: number
  fs: { t: number; ops: Record<string, number> }[]
}

const h = once<H>("hub", () => ({
  current: null,
  full: null,
  fullAt: 0,
  at: 0,
  error: null,
  inflight: null,
  fullInflight: null,
  events: null,
  backoff: 1000,
  debounce: null,
  timer: null,
  lastAsk: 0,
  fs: [],
}))

/** Fixture mode: counters that grow like a busy dev server's, an hour back from now. */
function fakeFs(base: Record<string, number>, t: number) {
  const k = (t - Date.now() + FS_KEEP_MS) / 1000
  return Object.fromEntries(Object.entries(base).map(([op, n], i) => [op, Math.round(n + (1 + i) * (1.2 * k - 300 * Math.cos(k / 300 + i) + 300 * Math.cos(i)))]))
}

function publish(st: State) {
  h.current = st
  h.at = Date.now()
  h.error = null
  const base = st.status?.fs_requests
  if (FIXTURE && base && !h.fs.length) for (let t = h.at - FS_KEEP_MS; t < h.at; t += FULL_MS) h.fs.push({ t, ops: fakeFs(base, t) })
  const ops = FIXTURE && base ? fakeFs(base, h.at) : base
  if (ops) {
    h.fs.push({ t: h.at, ops })
    const cut = h.at - FS_KEEP_MS
    while (h.fs.length && h.fs[0].t < cut) h.fs.shift()
  }
  broadcast("state", st)
}

function fail(e: unknown) {
  h.error = (e as Error).message
  broadcast("fail", { error: h.error })
}

/** A full `rekcod state`; concurrent callers share one run. */
export function refreshFull(): Promise<State> {
  h.fullInflight ??= readState(false)
    .then((st) => {
      h.full = st
      h.fullAt = Date.now()
      if (FIXTURE) fakeNodes(st)
      else recordNodes(st, h.fullAt)
      publish(st)
      return st
    })
    .catch((e) => {
      fail(e)
      throw e
    })
    .finally(() => (h.fullInflight = null))
  return h.fullInflight
}

/** `rekcod state --local` merged with the last full state's fleet parts. */
export function refreshLocal(): Promise<State> {
  if (FIXTURE || !h.full) return refreshFull()
  h.inflight ??= readState(true)
    .then((st) => {
      const merged = mergeLocal(st, h.full)
      publish(merged)
      return merged
    })
    .catch((e) => {
      fail(e)
      throw e
    })
    .finally(() => (h.inflight = null))
  return h.inflight
}

/** Refresh soon (after an action or an event); several calls in a row make one run. */
export function nudge() {
  if (h.debounce) clearTimeout(h.debounce)
  h.debounce = setTimeout(() => {
    h.debounce = null
    refreshLocal().catch(() => {})
  }, DEBOUNCE_MS)
}

/**
 * The last state says the VM runs. `rekcod events` and `rekcod stats` are docker passthrough
 * commands, which boot a stopped VM; an open page must never do that.
 */
export const vmRunning = () => h.current?.running === true

function followEvents() {
  if (FIXTURE || h.events || !vmRunning()) return
  const child = spawnRekcod(["events", "--format", "{{json .}}"])
  h.events = child
  let buf = ""
  child.stdout?.setEncoding("utf8")
  child.stdout?.on("data", (chunk: string) => {
    h.backoff = 1000
    const [lines, rest] = splitLines(buf + chunk)
    buf = rest
    for (const l of lines) {
      try {
        const e = JSON.parse(l)
        if (WATCH.has(e.Type)) nudge()
      } catch {}
    }
  })
  child.on("close", () => {
    if (h.events !== child) return
    h.events = null
    // Usually the VM stopped: show that, then try again, slower each time (up to 30 s).
    nudge()
    if (active()) setTimeout(followEvents, h.backoff)
    h.backoff = Math.min(h.backoff * 2, 30_000)
  })
}

const active = () => subscribers() > 0 || Date.now() - h.lastAsk < IDLE_MS

function loop() {
  if (!active()) {
    if (h.timer) clearInterval(h.timer)
    h.timer = null
    h.events?.kill()
    h.events = null
    return
  }
  if (Date.now() - h.fullAt >= FULL_MS) refreshFull().catch(() => {})
  if (!h.events) followEvents()
}

/** Starts the refresh loop and the events follower; each caller counts as a page watching. */
export function start() {
  h.lastAsk = Date.now()
  if (!h.timer) h.timer = setInterval(loop, 1000)
  followEvents()
}

/** The state for a polling client: the cache if fresh, else a refresh. */
export async function get(): Promise<State> {
  start()
  if (h.current && Date.now() - h.at < POLL_FRESH_MS) return h.current
  if (h.error && !h.current) return refreshFull()
  return Date.now() - h.fullAt >= FULL_MS ? refreshFull() : refreshLocal()
}

/** What's there now, without waiting (null before the first read). */
export const cached = () => h.current
export const lastError = () => h.error
export const fsHistory = () => h.fs
