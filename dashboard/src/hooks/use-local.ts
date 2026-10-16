"use client"

import { useCallback, useEffect, useState } from "react"

const EVENT = "rekcod:local"

function read<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key)
    return v == null ? fallback : (JSON.parse(v) as T)
  } catch {
    return fallback
  }
}

/**
 * A value kept in localStorage, shared by every component using the same key (and other tabs).
 * `fallback` until mounted, so server and first client render agree. Storage that throws
 * (private windows) just doesn't persist.
 */
export function useLocal<T>(key: string, fallback: T): [T, (v: T | ((old: T) => T)) => void] {
  const [value, setValue] = useState<T>(fallback)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setValue(read(key, fallback))
    const sync = (e: Event) => {
      if (e instanceof StorageEvent ? e.key === key : (e as CustomEvent).detail === key) setValue(read(key, fallback))
    }
    window.addEventListener("storage", sync)
    window.addEventListener(EVENT, sync)
    return () => {
      window.removeEventListener("storage", sync)
      window.removeEventListener(EVENT, sync)
    }
    // fallback is a literal at each call site; re-reading on its identity would loop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])
  const set = useCallback(
    (v: T | ((old: T) => T)) => {
      const next = typeof v === "function" ? (v as (old: T) => T)(read(key, fallback)) : v
      try {
        localStorage.setItem(key, JSON.stringify(next))
      } catch {}
      setValue(next)
      window.dispatchEvent(new CustomEvent(EVENT, { detail: key }))
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key],
  )
  return [value, set]
}
