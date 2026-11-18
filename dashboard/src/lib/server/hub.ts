import "server-only"
import type { ChildProcess } from "node:child_process"
import type { State } from "@/lib/types"
import { FIXTURE, readEngine, readState, spawnWatcher } from "@/lib/rekcod"
import { broadcast, once, subscribers } from "./bus"
import { engine, onEngine, setEngine } from "./engine"
import { mergeEngine, mergeLocal, unreachableEngine } from "./merge"
import { fakeNodes, recordNodes } from "./nodes"
import { splitLines } from "./stats"

// The state the page sees. Docker events refresh `rekcod state --local` (no controller calls,
// well under a second) and push it; a full `rekcod state` runs every 15 s for the fleet parts.
// Polling clients (`GET /api/state`) get the cache when it's under 3 s old. With a host's
// engine selected (engine.ts), its `rekcod host state` replaces the local docker parts and the
// events follower runs there.
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
  /** Bumped by `switchEngine`; a read started under another epoch is dropped. */
  epoch: number
  /** Aborts the selected host's `rekcod host state` (and its ssh) on a switch. */
  engineRead: AbortController
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
  epoch: 0,
  engineRead: new AbortController(),
}))

/** Fixture mode: counters that grow like a busy dev server's, an hour back from now. */
function fakeFs(base: Record<string, number>, t: number) {
  const k = (t - Date.now() + FS_KEEP_MS) / 1000
  return Object.fromEntries(Object.entries(base).map(([op, n], i) => [op, Math.round(n + (1 + i) * (1.2 * k - 300 * Math.cos(k / 300 + i) + 300 * Math.cos(i)))]))
}

function publish(st: State) {
  h.at = Date.now()
  st.read_at = h.at
  h.current = st
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

/** The selected host's engine merged with `local` (this Mac's last full state). */
async function withEngine(local: State): Promise<State> {
  const name = engine()
  if (name === "local") return local
  try {
    return mergeEngine(await readEngine(name, h.engineRead.signal), local, name)
  } catch (e) {
    return unreachableEngine(local, name, (e as Error).message)
  }
}

/** A full `rekcod state`; concurrent callers share one run. */
export function refreshFull(): Promise<State> {
  if (h.fullInflight) return h.fullInflight
  const epoch = h.epoch
  const p: Promise<State> = readState(false)
    .then(async (st) => {
      // This Mac's fleet parts, whichever engine is shown.
      h.full = st
      h.fullAt = Date.now()
      if (FIXTURE) fakeNodes(st)
      else recordNodes(st, h.fullAt)
      return withEngine(st)
    })
    .then(
      (shown) => settle(epoch, shown),
      (e) => settle(epoch, e as Error),
    )
    .finally(() => {
      if (h.fullInflight === p) h.fullInflight = null
    })
  h.fullInflight = p
  return p
}

/**
 * Publishes a read, or its failure, if the engine is still the one it was started for. A read
 * from before a switch is dropped, and its callers get the new engine's read instead.
 */
function settle(epoch: number, r: State | Error): Promise<State> | State {
  if (epoch !== h.epoch) return refreshFull()
  if (r instanceof Error) {
    fail(r)
    throw r
  }
  publish(r)
  return r
}

/** `rekcod state --local` merged with the last full state's fleet parts. */
export function refreshLocal(): Promise<State> {
  if (FIXTURE || !h.full) return refreshFull()
  if (h.inflight) return h.inflight
  const full = h.full
  const epoch = h.epoch
  const p: Promise<State> = (engine() === "local" ? readState(true).then((st) => mergeLocal(st, full)) : withEngine(full))
    .then(
      (merged) => settle(epoch, merged),
      (e) => settle(epoch, e as Error),
    )
    .finally(() => {
      if (h.inflight === p) h.inflight = null
    })
  h.inflight = p
  return p
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
  const child = spawnWatcher(onEngine(["events", "--format", "{{json .}}"]))
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
    // Nothing refreshes it now. A page that opens later must not act on it: it may still say
    // running after `rekcod stop`, and a docker command would boot the VM again.
    h.current = null
    h.at = 0
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

/**
 * Shows another engine: reads in flight are dropped (a host's is killed), the followers stop
 * (they run on the old one), the cached engine parts and the file-sharing counters go, and a
 * refresh starts. The fleet parts stay.
 */
export function switchEngine(name: string): Promise<State> {
  h.epoch++
  h.engineRead.abort()
  h.engineRead = new AbortController()
  setEngine(name)
  const old = h.events
  h.events = null
  old?.kill()
  h.current = null
  h.at = 0
  h.fullInflight = null
  h.inflight = null
  h.fs = []
  return refreshFull()
}

/** What's there now, without waiting (null before the first read). */
export const cached = () => h.current
export const lastError = () => h.error
export const fsHistory = () => h.fs
