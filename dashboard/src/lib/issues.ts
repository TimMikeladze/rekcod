import { ago } from "./format"
import type { State as S } from "./types"

export type Tab = "containers" | "images" | "volumes" | "apps" | "jobs" | "sandboxes" | "projects" | "ports" | "fleet"
type Tone = "ok" | "warn" | "bad" | "info" | "idle"

type Issue = { tone: Tone; text: string; tab: Tab }

/** Things worth looking at now, worst first. Empty means nothing is wrong. */
export function issues(state: S): Issue[] {
  const out: Issue[] = []
  for (const [what, err] of Object.entries(state.remote?.errors ?? {})) {
    const tab: Tab = what === "jobs" ? "jobs" : what === "apps" ? "apps" : "fleet"
    out.push({ tone: "bad", text: `Remote ${state.remote?.name ?? ""} ${what}: ${err}`.replace("  ", " "), tab })
  }
  for (const p of state.ports) if (p.error) out.push({ tone: "bad", text: `Port ${p.port} (${p.container}) isn't forwarded: ${p.error}`, tab: "ports" })
  for (const a of state.apps) {
    const latest = a.detail?.releases[0]
    if (latest?.state === "failed") out.push({ tone: "bad", text: `${a.name}: release #${latest.n} failed to deploy`, tab: "apps" })
    const lost = a.detail?.instances.filter((i) => i.state === "lost").length ?? 0
    if (lost) out.push({ tone: "bad", text: `${a.name}: ${lost} instance${lost > 1 ? "s" : ""} lost`, tab: "apps" })
    if (a.live_release == null && !a.sleeping) out.push({ tone: "warn", text: `${a.name} has no live release`, tab: "apps" })
  }
  for (const n of state.nodes ?? []) {
    if (state.generated - n.last_seen > 60) out.push({ tone: "bad", text: `Node ${n.id} last checked in ${ago(n.last_seen, state.generated)}`, tab: "fleet" })
    else if (n.mem_total_mib && (n.mem_free_mib ?? 0) / n.mem_total_mib < 0.1) out.push({ tone: "warn", text: `Node ${n.id} is low on memory`, tab: "fleet" })
  }
  const failed = state.jobs.filter((j) => (j.state === "failed" || j.state === "lost") && state.generated - j.created < 86_400)
  if (failed.length) out.push({ tone: "bad", text: `${failed.length} job${failed.length > 1 ? "s" : ""} failed in the last day`, tab: "jobs" })
  for (const c of state.containers)
    if (c.state === "restarting" || c.state === "dead") out.push({ tone: c.state === "dead" ? "bad" : "warn", text: `${c.name} is ${c.state}`, tab: "containers" })
  const d = state.status.disk
  if (d?.size && d.used_in_vm && d.used_in_vm / d.size > 0.85) out.push({ tone: "warn", text: `VM disk is ${Math.round((d.used_in_vm / d.size) * 100)}% full`, tab: "containers" })
  const rank = { bad: 0, warn: 1, info: 2, ok: 3, idle: 4 }
  return out.sort((a, b) => rank[a.tone] - rank[b.tone])
}

