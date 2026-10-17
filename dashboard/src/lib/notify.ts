import type { State } from "./types"

export const NOTIFY_KINDS = {
  release: "A release fails",
  instance: "An app instance is lost",
  node: "A node stops checking in",
  job: "A job fails",
  container: "A container exits non-zero",
} as const
export type NotifyKind = keyof typeof NOTIFY_KINDS

export type Notice = { key: string; kind: NotifyKind; title: string; body: string }

/** A node that hasn't sent a heartbeat for this long counts as stopped. */
const STALE_SECS = 60

/**
 * Everything notification-worthy in `s`, each with a key that stays the same for the same
 * event (so it's shown once): a failed release is `release:app#n`, a lost instance its
 * sandbox, a stale node its id and last heartbeat, a failed job its id, an exited container
 * its id and exit code.
 */
export function notices(s: State): Notice[] {
  const out: Notice[] = []
  for (const a of s.apps) {
    for (const r of a.detail?.releases ?? []) {
      if (r.state === "failed") out.push({ key: `release:${a.name}#${r.n}`, kind: "release", title: `${a.name}: release #${r.n} failed`, body: `commit ${r.commit.slice(0, 10)}; the previous release keeps serving` })
    }
    for (const i of a.detail?.instances ?? []) {
      if (i.state === "lost") out.push({ key: `instance:${i.sandbox}`, kind: "instance", title: `${a.name}: instance lost`, body: `${i.sandbox} on ${i.node}` })
    }
  }
  for (const n of s.nodes ?? []) {
    if (n.last_seen != null && s.generated - n.last_seen > STALE_SECS) out.push({ key: `node:${n.id}:${n.last_seen}`, kind: "node", title: `Node ${n.id} stopped checking in`, body: n.url ?? "" })
  }
  for (const j of s.jobs) {
    if (j.state === "failed" || j.state === "lost") {
      out.push({ key: `job:${j.id}`, kind: "job", title: `Job ${j.name ?? j.id} ${j.state}`, body: j.exit != null ? `exit ${j.exit}` : (j.node ?? "") })
    }
  }
  for (const c of s.containers) {
    const code = /^Exited \((\d+)\)/.exec(c.status)?.[1]
    if (c.state === "exited" && code && code !== "0") {
      out.push({ key: `container:${c.id}:${code}`, kind: "container", title: `${c.name} exited with ${code}`, body: c.image })
    }
  }
  return out
}

export type NotifySettings = { on: boolean; kinds: Record<NotifyKind, boolean> }
export const DEFAULT_SETTINGS: NotifySettings = { on: false, kinds: { release: true, instance: true, node: true, job: true, container: true } }

/** The notices in `s` not in `seen` and of an enabled kind; adds every key of `s` to `seen`. */
export function fresh(s: State, seen: Set<string>, settings: NotifySettings): Notice[] {
  const out: Notice[] = []
  for (const n of notices(s)) {
    if (seen.has(n.key)) continue
    seen.add(n.key)
    if (settings.on && settings.kinds[n.kind]) out.push(n)
  }
  return out
}
