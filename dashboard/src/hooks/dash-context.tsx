"use client"

import { createContext, useContext } from "react"
import type { ActivityEntry, Sample, Stat, State } from "@/lib/types"

/** Chart windows, in seconds. */
export const WINDOWS = [300, 900, 3600] as const
export type Window = (typeof WINDOWS)[number]

/** What `useDashboard` (hooks/use-dashboard.ts) keeps; every component reads it with `useDash()`. */
export type DashData = {
  state: State | null
  /** Why the last `rekcod state` failed (a stale binary is named here). */
  error: string | null
  /** Latest sample per container name. */
  stats: Record<string, Stat>
  /** Samples for the current window, oldest first. */
  history: Sample[]
  /** A colour per container name that stays the same across charts and reloads; "other" too. */
  colors: Record<string, string>
  window: Window
  setWindow: (w: Window) => void
  /** ms since the epoch of the last state that arrived. */
  loadedAt: number
  /** The event stream is connected (otherwise the page polls). */
  live: boolean
  refresh: () => void
  /** Actions this page ran, newest first. */
  activity: ActivityEntry[]
  markSeen: (ids: number[]) => void
  /** virtio-fs request counters over time, from each state the server read. */
  fsHistory: { t: number; ops: Record<string, number> }[]
}

export const DashContext = createContext<DashData | null>(null)

export function useDash(): DashData {
  const d = useContext(DashContext)
  if (!d) throw new Error("useDash outside DashContext")
  return d
}
