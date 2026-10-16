/**
 * `next`, with every part that deep-equals the same part of `prev` replaced by `prev`'s
 * object. Each state arrives as fresh JSON; sharing keeps unchanged rows' references, so
 * memoised rows and `useMemo` deps skip work when nothing in them changed.
 */
export function share<T>(prev: unknown, next: T): T {
  if (prev === next) return next
  if (Array.isArray(prev) && Array.isArray(next)) {
    let same = prev.length === next.length
    const out = next.map((v, i) => {
      const s = share(prev[i], v)
      if (s !== prev[i]) same = false
      return s
    })
    return (same ? prev : out) as T
  }
  if (isObj(prev) && isObj(next)) {
    const keys = Object.keys(next)
    let same = keys.length === Object.keys(prev).length
    const out: Record<string, unknown> = {}
    for (const k of keys) {
      const s = share(prev[k], next[k])
      if (s !== prev[k] || !(k in prev)) same = false
      out[k] = s
    }
    return (same ? prev : out) as T
  }
  return next
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v)
