import "server-only"

// The routes run commands as you, so only same-origin requests from a localhost page get
// through: the Host check stops DNS rebinding, the custom header forces a CORS preflight that
// other sites' pages fail.
const LOCAL = /^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/

export function refuse(req: Request, mutating: boolean): Response | null {
  const host = req.headers.get("host") ?? ""
  if (!LOCAL.test(host)) return new Response("dashboard answers on localhost only", { status: 403 })
  const origin = req.headers.get("origin")
  if (origin && new URL(origin).host !== host) return new Response("cross-origin request", { status: 403 })
  if (mutating && req.headers.get("x-rekcod") !== "1") return new Response("missing x-rekcod header", { status: 403 })
  return null
}
