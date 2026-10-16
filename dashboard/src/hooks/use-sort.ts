"use client"

import { usePathname, useRouter, useSearchParams } from "next/navigation"

export type SortKey<T> = (row: T) => string | number | null | undefined

/** `?sort=mem` ascending, `?sort=-mem` descending; anything not in `keys` is ignored. */
export function parseSort(raw: string | null, keys: readonly string[]): { key: string; desc: boolean } | null {
  if (!raw) return null
  const desc = raw.startsWith("-")
  const key = desc ? raw.slice(1) : raw
  return keys.includes(key) ? { key, desc } : null
}

/** Rows sorted by one accessor; nulls last either way, ties keep their order. */
export function sortRows<T>(rows: T[], by: SortKey<T>, desc: boolean): T[] {
  return rows
    .map((r, i) => ({ r, i, v: by(r) }))
    .sort((a, b) => {
      if (a.v == null || b.v == null) return a.v == null ? (b.v == null ? a.i - b.i : 1) : -1
      const c = typeof a.v === "number" && typeof b.v === "number" ? a.v - b.v : String(a.v).localeCompare(String(b.v), undefined, { numeric: true })
      return (desc ? -c : c) || a.i - b.i
    })
    .map((x) => x.r)
}

/** What a sortable header shows and does. */
export type SortState = { key: string | null; desc: boolean; toggle: (key: string) => void }

/**
 * The list's sort, kept in the URL next to `?q=`. A header click goes ascending, descending,
 * then back to the list's own order. Needs a Suspense boundary (it reads the search params).
 */
export function useSort<T>(keys: Record<string, SortKey<T>>): SortState & { apply: (rows: T[]) => T[] } {
  const params = useSearchParams()
  const router = useRouter()
  const pathname = usePathname()
  const cur = parseSort(params.get("sort"), Object.keys(keys))
  const toggle = (key: string) => {
    const p = new URLSearchParams(params.toString())
    const next = cur?.key !== key ? key : !cur.desc ? `-${key}` : null
    if (next) p.set("sort", next)
    else p.delete("sort")
    const qs = p.toString()
    router.replace(qs ? `${pathname}?${qs}` : pathname, { scroll: false })
  }
  const by = cur ? keys[cur.key] : null
  const desc = cur?.desc ?? false
  return { key: cur?.key ?? null, desc, toggle, apply: (rows: T[]) => (by ? sortRows(rows, by, desc) : rows) }
}
