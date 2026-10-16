"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import type { DashData, Window } from "@/hooks/dash-context"
import { share } from "@/lib/share"
import type { ActivityEntry, Sample, Stat, State, StatsResponse } from "@/lib/types"

const POLL_MS = 3000
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
    if (ops) {
      const t = Date.now()
      setFsHistory((h) => [...h.filter((x) => x.t > t - FS_KEEP_MS), { t, ops }])
    }
  }, [])

  const refresh = useCallback(async () => {
    if (busy.current) return
    busy.current = true
    try {
      const r = await fetch("/api/state", { cache: "no-store" })
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
      const r: StatsResponse = await fetch(`/api/stats?window=${w}`, { cache: "no-store" }).then((r) => r.json())
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
    let es: EventSource | null = null
    let poll: ReturnType<typeof setInterval> | null = null
    let stopped = false
    const startPolling = () => {
      poll ??= setInterval(() => document.visibilityState === "visible" && refresh(), POLL_MS)
    }
    const stopPolling = () => {
      if (poll) clearInterval(poll)
      poll = null
    }
    const connect = () => {
      if (stopped) return
      es = new EventSource("/api/events")
      es.onopen = () => {
        setLive(true)
        stopPolling()
      }
      es.onerror = () => {
        setLive(false)
        startPolling()
      }
      es.addEventListener("state", (e) => gotState(JSON.parse((e as MessageEvent).data)))
      es.addEventListener("fail", (e) => setError(JSON.parse((e as MessageEvent).data).error))
      es.addEventListener("stats", (e) => {
        const d = JSON.parse((e as MessageEvent).data) as { latest: Stat[]; sample: Sample[]; colors: Record<string, string> }
        setStats((old) => sameStats(old, d.latest))
        setColors((old) => share(old, d.colors))
        if (d.sample.length) {
          const cut = Date.now() - windowRef.current * 1000
          setHistory((h) => [...h.filter((s) => s.t > cut), ...d.sample])
        }
      })
      es.addEventListener("activity", (e) => {
        const a = JSON.parse((e as MessageEvent).data) as ActivityEntry
        setActivity((list) => [a, ...list.filter((x) => x.id !== a.id)])
      })
    }
    const first = setTimeout(() => {
      refresh()
      loadStats(windowRef.current)
      fetch("/api/activity", { cache: "no-store" })
        .then((r) => r.json())
        .then((l: ActivityEntry[]) => Array.isArray(l) && setActivity(l))
        .catch(() => {})
      fetch("/api/vm-history", { cache: "no-store" })
        .then((r) => r.json())
        .then((v: { fs?: DashData["fsHistory"] }) => v.fs && setFsHistory((h) => [...v.fs!, ...h.filter((x) => x.t > (v.fs!.at(-1)?.t ?? 0))]))
        .catch(() => {})
      connect()
    }, 0)
    return () => {
      stopped = true
      clearTimeout(first)
      stopPolling()
      es?.close()
    }
  }, [refresh, loadStats, gotState])

  const markSeen = useCallback((ids: number[]) => {
    if (!ids.length) return
    const want = new Set(ids)
    setActivity((list) => list.map((a) => (want.has(a.id) ? { ...a, seen: true } : a)))
    fetch("/api/activity", {
      method: "POST",
      headers: { "content-type": "application/json", "x-rekcod": "1" },
      body: JSON.stringify({ seen: ids }),
    }).catch(() => {})
  }, [])

  return { state, error, stats, history, colors, window, setWindow, loadedAt, live, refresh, activity, markSeen, fsHistory }
}
