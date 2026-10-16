import "server-only"
import type { Node, State } from "@/lib/types"
import { once } from "./bus"

/** One node at time `t` (ms), from a full state. Mirrors `NodeSample` in fleet-charts.tsx. */
export type NodeSample = { t: number; load: number | null; mem_used_mib: number | null; disk_used_gib: number | null }

const KEEP_MS = 3600_000
/** Fixture mode makes an hour of samples at the full-refresh rate. */
const STEP_MS = 15_000

const h = once("nodes", () => ({ by: new Map<string, NodeSample[]>() }))

export function sampleOf(n: Node, t: number): NodeSample {
  return {
    t,
    load: n.load ?? null,
    mem_used_mib: n.mem_total_mib != null && n.mem_free_mib != null ? n.mem_total_mib - n.mem_free_mib : null,
    disk_used_gib: n.disk_total_gib != null && n.disk_free_gib != null ? n.disk_total_gib - n.disk_free_gib : null,
  }
}

/** Keeps each node's load, memory and disk from a full `rekcod state`, the last hour. */
export function recordNodes(s: State, t = Date.now()) {
  for (const n of s.nodes ?? []) {
    const list = h.by.get(n.id) ?? []
    list.push(sampleOf(n, t))
    while (list.length && list[0].t < t - KEEP_MS) list.shift()
    h.by.set(n.id, list)
  }
}

/** Fixture mode: an hour of wandering values around each node's current ones. */
export function fakeNodes(s: State, now = Date.now()) {
  if (h.by.size) return
  for (const n of s.nodes ?? []) {
    const list: NodeSample[] = []
    const seed = [...n.id].reduce((a, c) => a + c.charCodeAt(0), 0)
    for (let t = now - KEEP_MS; t <= now; t += STEP_MS) {
      const k = (t - now) / 60_000
      const wave = Math.sin(k / 7 + seed) * 0.5 + Math.sin(k / 2.3 + seed * 2) * 0.2
      const s0 = sampleOf(n, t)
      list.push({
        t,
        load: s0.load == null ? null : Math.max(0, s0.load * (1 + wave)),
        mem_used_mib: s0.mem_used_mib == null ? null : Math.max(0, s0.mem_used_mib * (1 + wave / 6)),
        disk_used_gib: s0.disk_used_gib == null ? null : s0.disk_used_gib - (k < -30 ? 2 : 0),
      })
    }
    h.by.set(n.id, list)
  }
}

export const nodeHistory = (id: string) => h.by.get(id) ?? []
