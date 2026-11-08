"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import type { DashData, Window } from "@/hooks/dash-context"
import { api } from "@/lib/api"
import { share } from "@/lib/share"
import { readEvents } from "@/lib/sse"
import type { ActivityEntry, Sample, Stat, State, StatsResponse } from "@/lib/types"

const POLL_MS = 3000
// What EventSource waits before reconnecting.
const RETRY_MS = 3000
const FS_KEEP_MS = 3600_000

const byName = (list: Stat[]) => Object.fromEntries(list.map((s) => [s.name, s]))
const sameStats = (old: Record<string, Stat>, list: Stat[]) => share(old, byName(list))

/**
 * Everything the page shows. `/api/events` pushes state, stats and activity; while it's down
 * the hook polls `/api/state` every 3 s (only while the tab is visible).
 */
export function useDashboard(): DashData {
  const [state, setState] = useState<State | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [stats, setStats] = useState<Record<string, Stat>>({})
  const [history, setHistory] = useState<Sample[]>([])
  const [colors, setColors] = useState<Record<string, string>>({})
  const [window, setWindowState] = useState<Window>(300)
  const [loadedAt, setLoadedAt] = useState(0)
  const [live, setLive] = useState(false)
  const [activity, setActivity] = useState<ActivityEntry[]>([])
  const [fsHistory, setFsHistory] = useState<DashData["fsHistory"]>([])
  const windowRef = useRef<Window>(300)
  const busy = useRef(false)

  const gotState = useCallback((s: State) => {
    // Unchanged rows keep their objects, so memoised rows skip rendering.
    setState((old) => share(old, s))
    setError(null)
    setLoadedAt(Date.now())
    const ops = s.status?.fs_requests
    // Stamp with the server's read time: a cached answer repeats it and adds no sample (a fresh
    // timestamp on old counters would chart a 0 req/s dip, then a spike).
    const t = s.read_at
    if (ops && t) setFsHistory((h) => (h.some((x) => x.t === t) ? h : [...h.filter((x) => x.t > t - FS_KEEP_MS), { t, ops }]))
  }, [])

  const refresh = useCallback(async () => {
    if (busy.current) return
    busy.current = true
    try {
      const r = await api("/api/state")
      const body = await r.json()
      if (!r.ok) throw new Error(body.error ?? r.statusText)
      gotState(body)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      busy.current = false
    }
  }, [gotState])

  const loadStats = useCallback(async (w: Window) => {
    try {
      const r: StatsResponse = await api(`/api/stats?window=${w}`).then((r) => r.json())
      if (windowRef.current !== w) return
      setStats((old) => sameStats(old, r.latest))
      setHistory(r.history)
      setColors((old) => share(old, r.colors))
    } catch {}
  }, [])

  const setWindow = useCallback(
    (w: Window) => {
      windowRef.current = w
      setWindowState(w)
      loadStats(w)
    },
    [loadStats],
  )

  // The event stream, with polling while it's down.
  useEffect(() => {
    let ctl: AbortController | null = null
    let retry: ReturnType<typeof setTimeout> | undefined
    let poll: ReturnType<typeof setInterval> | null = null
    let stopped = false
    const startPolling = () => {
      poll ??= setInterval(() => document.visibilityState === "visible" && refresh(), POLL_MS)
    }
    const stopPolling = () => {
      if (poll) clearInterval(poll)
      poll = null
    }
    const onEvent = (event: string, data: string) => {
      const d = JSON.parse(data)
      if (event === "state") gotState(d as State)
      else if (event === "fail") setError((d as { error: string }).error)
      else if (event === "stats") {
        const s = d as { latest: Stat[]; sample: Sample[]; colors: Record<string, string> }
        setStats((old) => sameStats(old, s.latest))
        setColors((old) => share(old, s.colors))
        if (s.sample.length) {
          const cut = Date.now() - windowRef.current * 1000
          setHistory((h) => [...h.filter((x) => x.t > cut), ...s.sample])
        }
      } else if (event === "activity") {
        const a = d as ActivityEntry
        setActivity((list) => [a, ...list.filter((x) => x.id !== a.id)])
      }
    }
    const connect = async () => {
      if (stopped) return
      ctl = new AbortController()
      try {
        const r = await api("/api/events", { signal: ctl.signal })
        // Signed out: the shell swaps this page for the sign-in one.
        if (r.status === 401) return
        if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`)
        setLive(true)
        stopPolling()
        await readEvents(r.body, onEvent)
      } catch {}
      if (stopped) return
      setLive(false)
      startPolling()
      retry = setTimeout(connect, RETRY_MS)
    }
    const first = setTimeout(() => {
      refresh()
      loadStats(windowRef.current)
      api("/api/activity")
        .then((r) => r.json())
        .then((l: ActivityEntry[]) => Array.isArray(l) && setActivity(l))
        .catch(() => {})
      api("/api/vm-history")
        .then((r) => r.json())
        .then((v: { fs?: DashData["fsHistory"] }) => v.fs && setFsHistory((h) => [...v.fs!, ...h.filter((x) => x.t > (v.fs!.at(-1)?.t ?? 0))]))
        .catch(() => {})
      connect()
    }, 0)
    return () => {
      stopped = true
      clearTimeout(first)
      clearTimeout(retry)
      stopPolling()
      ctl?.abort()
    }
  }, [refresh, loadStats, gotState])

  const markSeen = useCallback((ids: number[]) => {
    if (!ids.length) return
    const want = new Set(ids)
    setActivity((list) => list.map((a) => (want.has(a.id) ? { ...a, seen: true } : a)))
    api("/api/activity", {
      method: "POST",
      headers: { "content-type": "application/json", "x-rekcod": "1" },
      body: JSON.stringify({ seen: ids }),
    }).catch(() => {})
  }, [])

  return { state, error, stats, history, colors, window, setWindow, loadedAt, live, refresh, activity, markSeen, fsHistory }
}
