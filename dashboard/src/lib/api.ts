// Every request the page makes to /api goes through `api()`, which adds the token. The token
// lives in localStorage, which is per origin (scheme, host and port), so another localhost
// server never sees it; a cookie would reach all of them (docs/dashboard.md).

/** The localStorage key `/auth` (proxy.ts) writes the token to. */
export const TOKEN_KEY = "rekcod.token"

/** `in`: there's a token; `out`: there's none; `stale`: the server answered 401 to it (it restarted with a new one). */
export type AuthStatus = "in" | "out" | "stale"

const listeners = new Set<() => void>()
// Kept instead of removing the token: another tab may have just stored a newer one.
let rejected: string | null = null

function stored(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY)
  } catch {
    return null
  }
}

export function authStatus(): AuthStatus {
  const t = stored()
  return !t ? "out" : t === rejected ? "stale" : "in"
}

/** Calls `onChange` when the status may have changed: a 401 here, or a sign-in in another tab. */
export function subscribeAuth(onChange: () => void): () => void {
  const onStorage = (e: StorageEvent) => (e.key === TOKEN_KEY || e.key === null) && onChange()
  listeners.add(onChange)
  window.addEventListener("storage", onStorage)
  return () => {
    listeners.delete(onChange)
    window.removeEventListener("storage", onStorage)
  }
}

/** `fetch` with `Authorization: Bearer <token>` and no caching. Without a usable token it answers 401 itself. */
export async function api(path: string, init: RequestInit = {}): Promise<Response> {
  const t = stored()
  if (!t || t === rejected) return new Response("not signed in; open the dashboard with `rekcod dashboard`", { status: 401 })
  const headers = new Headers(init.headers)
  headers.set("authorization", `Bearer ${t}`)
  const r = await fetch(path, { cache: "no-store", ...init, headers })
  if (r.status === 401 && rejected !== t) {
    rejected = t
    for (const l of listeners) l()
  }
  return r
}
