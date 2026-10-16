// The per-launch token (docs/dashboard.md). `rekcod dashboard` and `npm run dev` put it in
// REKCOD_DASHBOARD_TOKEN; `/auth?token=` trades it for an HttpOnly cookie, and the proxy and
// every API route check that cookie. No "server-only" here: proxy.ts imports this too.
import { createHash, timingSafeEqual } from "node:crypto"

export const TOKEN_ENV = "REKCOD_DASHBOARD_TOKEN"
export const LOCAL_HOST = /^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/

/** The server's token, or null when it has none (or one too short to count). Every request is then refused. */
export function serverToken(): string | null {
  const t = process.env[TOKEN_ENV] ?? ""
  return t.length >= 32 ? t : null
}

/** Compares in constant time. Hashing first makes the lengths equal, which timingSafeEqual needs. */
export function sameSecret(given: string, expected: string): boolean {
  const h = (s: string) => createHash("sha256").update(s).digest()
  return timingSafeEqual(h(given), h(expected))
}

/**
 * Cookies ignore ports, so two dashboards on localhost would overwrite one name. Each port gets
 * its own.
 */
export function cookieName(host: string): string {
  return `rekcod-dashboard-${host.match(/:(\d+)$/)?.[1] ?? "80"}`
}

function readCookie(header: string | null, name: string): string | null {
  for (const part of (header ?? "").split(";")) {
    const i = part.indexOf("=")
    if (i > 0 && part.slice(0, i).trim() === name) return part.slice(i + 1).trim()
  }
  return null
}

/** True when the request carries this server's token in its cookie. */
export function authorized(req: Request): boolean {
  const token = serverToken()
  const given = readCookie(req.headers.get("cookie"), cookieName(req.headers.get("host") ?? ""))
  return !!token && !!given && sameSecret(given, token)
}

/** A session cookie: it lasts until the browser quits, and the token until the server restarts. */
export function authCookie(host: string, token: string): string {
  return `${cookieName(host)}=${token}; Path=/; HttpOnly; SameSite=Strict`
}
