import "server-only"
import type { ChildProcess } from "node:child_process"
import type { Sample, Stat, StatsResponse } from "@/lib/types"
import { FIXTURE, readState, spawnRekcod } from "@/lib/rekcod"
import { broadcast, once, subscribers } from "./bus"
import { vmRunning } from "./hub"
import { Colors, fakeSample, parseStatLine, Ring, SAMPLE_SECS, splitLines, KEEP } from "./stats"

// One `rekcod stats` child for the whole server: `docker stats --no-stream` waits ~2 s for a
// sample on every call, the stream keeps one ready. It's stopped after a minute in which no
// page asked (an open event stream counts as asking).
const IDLE_MS = 60_000

type S = {
  child: ChildProcess | null
  latest: Map<string, Stat & { at: number }>
  ring: Ring
  colors: Colors
  lastAsk: number
  timer: ReturnType<typeof setInterval> | null
  fixtureNames: string[] | null
}

const s = once<S>("sampler", () => ({
  child: null,
  latest: new Map(),
  ring: new Ring(),
  colors: new Colors(),
  lastAsk: 0,
  timer: null,
  fixtureNames: null,
}))

function spawnChild() {
  if (!vmRunning()) return
  const child = spawnRekcod(["stats", "--format", "{{json .}}"])
  s.child = child
  let buf = ""
  child.stdout?.setEncoding("utf8")
  child.stdout?.on("data", (chunk: string) => {
    const [lines, rest] = splitLines(buf + chunk)
    buf = rest
    const now = Date.now()
    for (const l of lines) {
      const st = parseStatLine(l)
      if (st) s.latest.set(st.name, { ...st, at: now })
    }
  })
  child.on("close", () => {
    if (s.child === child) s.child = null
  })
}

/** Names of the fixture's running containers, and an hour of history for them. */
async function prefillFixture() {
  const st = await readState()
  const names = st.containers.filter((c) => c.state === "running").map((c) => c.name)
  s.fixtureNames = names
  s.colors.assign(names)
  const now = Date.now()
  for (let i = KEEP - 1; i >= 0; i--) {
    const t = now - i * SAMPLE_SECS * 1000
    s.ring.push(names.map((n) => fakeSample(n, t)))
  }
}

function fixtureLatest(t: number) {
  for (const n of s.fixtureNames ?? []) {
    const f = fakeSample(n, t)
    s.latest.set(n, { name: n, cpu: f.cpu, mem: f.mem, memLimit: 8 * 2 ** 30, netIn: 0, netOut: 0, at: t })
  }
}

function tick() {
  const now = Date.now()
  if (FIXTURE) fixtureLatest(now)
  // Containers that stopped drop out of the stream; their last sample goes stale.
  const fresh = [...s.latest.values()].filter((x) => now - x.at < SAMPLE_SECS * 3000)
  for (const x of s.latest.values()) if (now - x.at >= SAMPLE_SECS * 3000) s.latest.delete(x.name)
  const sample: Sample[] = fresh.map((x) => ({ t: now, name: x.name, cpu: x.cpu, mem: x.mem }))
  s.colors.assign(sample.map((x) => x.name))
  if (sample.length) s.ring.push(sample)
  broadcast("stats", { latest: latest(), sample, colors: s.colors.map() })
  if (subscribers() === 0 && now - s.lastAsk > IDLE_MS) stop()
  // The child exits when the VM stops; try again each tick while someone's watching.
  else if (!FIXTURE && !s.child) spawnChild()
}

function stop() {
  s.child?.kill()
  s.child = null
  if (s.timer) clearInterval(s.timer)
  s.timer = null
}

/** Starts sampling if it isn't running. Every caller counts as a page that wants stats. */
export async function ensure() {
  s.lastAsk = Date.now()
  if (FIXTURE) {
    if (!s.fixtureNames) await prefillFixture()
    fixtureLatest(Date.now())
  } else if (!s.child) {
    spawnChild()
  }
  if (!s.timer) s.timer = setInterval(tick, SAMPLE_SECS * 1000)
}

function latest(): Stat[] {
  return [...s.latest.values()].map((x) => ({ name: x.name, cpu: x.cpu, mem: x.mem, memLimit: x.memLimit, netIn: x.netIn, netOut: x.netOut }))
}

export async function stats(window: number): Promise<StatsResponse> {
  await ensure()
  return { latest: latest(), history: s.ring.window(window), colors: s.colors.map(), window }
}
