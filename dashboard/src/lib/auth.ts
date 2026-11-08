// The per-launch token (docs/dashboard.md). `rekcod dashboard` and `npm run dev` put it in
// REKCOD_DASHBOARD_TOKEN; `/auth?token=` stores it in the page's localStorage, and every API
// request sends it as `Authorization: Bearer`. Never a cookie: cookies ignore ports, so every
// other server on localhost would receive it. No "server-only" here: proxy.ts imports this too.
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

/** True when the request's `Authorization: Bearer` header carries this server's token. Cookies don't count. */
export function authorized(req: Request): boolean {
  const token = serverToken()
  const given = req.headers.get("authorization")?.match(/^Bearer +(\S+)$/i)?.[1]
  return !!token && !!given && sameSecret(given, token)
}

// Builds before the header kept the token in `rekcod-dashboard-PORT`, HttpOnly on localhost.
const LEGACY_COOKIE = /^rekcod-dashboard-\d+$/

/** `Set-Cookie` values that delete every old sign-in cookie the request carried (any port's: they all reach every port). */
export function expireLegacyCookies(cookie: string | null): string[] {
  const names = new Set<string>()
  for (const part of (cookie ?? "").split(";")) {
    const i = part.indexOf("=")
    const name = part.slice(0, i).trim()
    if (i > 0 && LEGACY_COOKIE.test(name)) names.add(name)
  }
  return [...names].map((n) => `${n}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict`)
}
