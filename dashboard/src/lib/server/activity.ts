import "server-only"
import type { ActivityEntry } from "@/lib/types"
import { broadcast, once } from "./bus"

/** Output kept per entry, from the end. */
const TAIL = 4096
const KEEP = 500

const log = once("activity", () => ({ next: 1, list: [] as ActivityEntry[] }))

const tail = (s: string) => (s.length > TAIL ? s.slice(-TAIL) : s)

/** Records one run, newest first. Successful runs start seen; failed ones stay red until opened. */
export function record(e: Omit<ActivityEntry, "id" | "seen">): ActivityEntry {
  const entry: ActivityEntry = { ...e, id: log.next++, stdout: tail(e.stdout), stderr: tail(e.stderr), seen: e.code === 0 }
  log.list.unshift(entry)
  log.list.length = Math.min(log.list.length, KEEP)
  broadcast("activity", entry)
  return entry
}

export const list = () => log.list

export function markSeen(ids: number[]) {
  const want = new Set(ids)
  for (const e of log.list) if (want.has(e.id)) e.seen = true
}
