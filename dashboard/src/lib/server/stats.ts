// `docker stats` parsing, the sample ring buffer and the stable colour map. No Node APIs here,
// so the tests import it directly; the child process lives in sampler.ts.
import { parseBytes } from "@/lib/format"
import type { Sample, Stat } from "@/lib/types"

/** Seconds between samples, and how many are kept (an hour). */
export const SAMPLE_SECS = 5
export const KEEP = 3600 / SAMPLE_SECS

// The charts' five series colours (they show the top five); "other" is gray. Slot 6 reuses slot 1.
export const PALETTE = ["#2a78d6", "#d9822b", "#2e9e6b", "#b8459a", "#8a6fd1"]
export const OTHER = "#7a7f87"

// Streaming `docker stats` redraws the screen: ESC[H ESC[K before a frame, ESC[J after.
const ANSI = /\x1b\[[0-9;?]*[A-Za-z]/g

/** One `{{json .}}` line → a Stat, or null for blank lines and redraw codes. */
export function parseStatLine(line: string): Stat | null {
  const l = line.replace(ANSI, "").trim()
  if (!l.startsWith("{")) return null
  try {
    const s = JSON.parse(l)
    const [mem, limit] = String(s.MemUsage ?? "").split("/")
    const [netIn, netOut] = String(s.NetIO ?? "").split("/")
    // A container with no reading yet prints "--" everywhere; that's no sample, not 0%.
    const cpu = parseFloat(s.CPUPerc)
    const used = parseBytes(mem ?? "")
    if (!Number.isFinite(cpu) || used == null) return null
    return {
      name: String(s.Name),
      cpu,
      mem: used,
      memLimit: parseBytes(limit ?? "") ?? 0,
      netIn: parseBytes(netIn ?? "") ?? 0,
      netOut: parseBytes(netOut ?? "") ?? 0,
    }
  } catch {
    return null
  }
}

/** Splits a chunk of the stats stream into complete lines; returns [lines, leftover]. */
export function splitLines(buf: string): [string[], string] {
  const parts = buf.split("\n")
  const rest = parts.pop() ?? ""
  return [parts, rest]
}

/** Fixed-size buffer of sample ticks (one array of samples per tick), oldest first. */
export class Ring {
  ticks: Sample[][] = []
  constructor(readonly keep = KEEP) {}
  push(tick: Sample[]) {
    this.ticks.push(tick)
    if (this.ticks.length > this.keep) this.ticks.splice(0, this.ticks.length - this.keep)
  }
  /** Samples newer than `now - secs`, flattened, oldest first. */
  window(secs: number, now = Date.now()): Sample[] {
    const since = now - secs * 1000
    return this.ticks.flat().filter((s) => s.t > since)
  }
}

/**
 * A palette slot per name, assigned the first time it's seen and never changed. A batch of
 * new names is assigned in sorted order, each taking the lowest slot no live name holds.
 * Past the palette's length, slots repeat.
 */
export class Colors {
  private slots = new Map<string, number>()
  assign(names: Iterable<string>) {
    const fresh = [...new Set(names)].filter((n) => n !== "other" && !this.slots.has(n)).sort()
    for (const n of fresh) {
      const used = new Set(this.slots.values())
      let i = 0
      while (used.has(i)) i++
      this.slots.set(n, i)
    }
  }
  map(): Record<string, string> {
    const out: Record<string, string> = { other: OTHER }
    for (const [n, i] of this.slots) out[n] = PALETTE[i % PALETTE.length]
    return out
  }
}

/** Plausible CPU/memory for a fixture container at time t: a slow wave plus noise. */
export function fakeSample(name: string, t: number): Sample {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  const base = 2 + (h % 30)
  const phase = (h % 100) / 10
  const wave = Math.sin(t / 240_000 + phase) * base * 0.4
  // Hash of the 5 s tick: independent per sample, the same for the same tick.
  let n = (Math.floor(t / 5000) ^ h) >>> 0
  n = Math.imul(n ^ (n >>> 16), 0x45d9f3b) >>> 0
  n = Math.imul(n ^ (n >>> 16), 0x45d9f3b) >>> 0
  const noise = ((n ^ (n >>> 16)) >>> 0) / 0xffffffff - 0.5
  const cpu = Math.max(0.1, base + wave + noise * base * 0.3)
  const memBase = (64 + (h % 900)) * 1024 * 1024
  const mem = memBase * (1 + 0.08 * Math.sin(t / 900_000 + phase) + 0.02 * noise)
  return { t, name, cpu: Math.round(cpu * 100) / 100, mem: Math.round(mem) }
}
