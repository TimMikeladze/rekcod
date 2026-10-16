import { NextResponse, type NextRequest } from "next/server"
import { authCookie, authorized, LOCAL_HOST, sameSecret, serverToken } from "@/lib/auth"

// Every page and API route needs the token's cookie (lib/auth.ts). `/auth?token=` sets it.
export function proxy(req: NextRequest) {
  const host = req.headers.get("host") ?? ""
  if (!LOCAL_HOST.test(host)) return new Response("dashboard answers on localhost only", { status: 403 })
  const { pathname, searchParams } = req.nextUrl
  if (pathname === "/auth") {
    const token = serverToken()
    const given = searchParams.get("token")
    if (!token || !given || !sameSecret(given, token)) return signIn(token ? "That link's token is wrong or from an earlier launch." : "This server was started without a token.")
    // Back to the Host the browser used (checked above), which is the host the cookie is for;
    // req.nextUrl can carry the bound address instead.
    return new Response(null, {
      status: 303,
      headers: { location: `http://${host}/`, "set-cookie": authCookie(host, token), "cache-control": "no-store", "referrer-policy": "no-referrer" },
    })
  }
  if (authorized(req)) return NextResponse.next()
  if (pathname.startsWith("/api/")) return new Response("not signed in; open the dashboard with `rekcod dashboard`", { status: 401 })
  return signIn("")
}

function signIn(why: string) {
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Sign in · rekcod</title>
<style>:root{color-scheme:light dark}body{font:15px/1.5 system-ui,sans-serif;max-width:34rem;margin:15vh auto;padding:0 16px}code{font:13px ui-monospace,monospace;padding:2px 4px;border-radius:4px;background:#8882}</style></head>
<body><h1>Open this with <code>rekcod dashboard</code></h1><p>The dashboard runs commands as you, so it needs the sign-in link that <code>rekcod dashboard</code> opens (or <code>npm run dev</code> prints). Each launch has a new one.</p>${why ? `<p>${why}</p>` : ""}</body></html>`
  return new Response(html, { status: 401, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } })
}

// Static build files and icons carry nothing private; the browser fetches the manifest
// without cookies.
export const config = {
  matcher: ["/((?!_next/static|_next/image|icon\\.svg|apple-icon\\.png|manifest\\.webmanifest|favicon\\.ico).*)"],
}
