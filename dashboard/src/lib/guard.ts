import "server-only"
import { authorized, LOCAL_HOST } from "./auth"

// The routes run commands as you. proxy.ts already checks the token for every request; this
// checks it again so a route stays closed if the proxy's matcher ever misses it, then:
// the Host check stops DNS rebinding, Sec-Fetch-Site and Origin stop other pages (another
// localhost port is "same-site" and gets the SameSite=Strict cookie), and the custom header
// forces a CORS preflight that other sites' pages fail.
export function refuse(req: Request, mutating: boolean): Response | null {
  const host = req.headers.get("host") ?? ""
  if (!LOCAL_HOST.test(host)) return new Response("dashboard answers on localhost only", { status: 403 })
  if (!authorized(req)) return new Response("not signed in; open the dashboard with `rekcod dashboard`", { status: 401 })
  const site = req.headers.get("sec-fetch-site")
  if (site && site !== "same-origin" && site !== "none") return new Response("cross-site request", { status: 403 })
  const origin = req.headers.get("origin")
  if (origin !== null && originHost(origin) !== host) return new Response("cross-origin request", { status: 403 })
  if (mutating && req.headers.get("x-rekcod") !== "1") return new Response("missing x-rekcod header", { status: 403 })
  return null
}

// "null" (sandboxed frames, file://) and anything unparsable match no host.
function originHost(origin: string): string | null {
  try {
    return new URL(origin).host || null
  } catch {
    return null
  }
}
