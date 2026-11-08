import { NextResponse, type NextRequest } from "next/server"
import { TOKEN_KEY } from "@/lib/api"
import { authorized, expireLegacyCookies, LOCAL_HOST, sameSecret, serverToken } from "@/lib/auth"

// Every API route needs `Authorization: Bearer <token>` (lib/auth.ts); `/auth?token=` puts the
// token where the page's `api()` finds it. Pages are public: they're shells that fetch
// everything they show from the API.
export function proxy(req: NextRequest) {
  const host = req.headers.get("host") ?? ""
  if (!LOCAL_HOST.test(host)) return new Response("dashboard answers on localhost only", { status: 403 })
  const expire = expireLegacyCookies(req.headers.get("cookie"))
  const { pathname, searchParams } = req.nextUrl
  if (pathname === "/auth") {
    const token = serverToken()
    const given = searchParams.get("token")
    if (!token || !given || !sameSecret(given, token)) return signIn(token ? "That link's token is wrong or from an earlier launch." : "This server was started without a token.", expire)
    return page(200, AUTH_PAGE, expire)
  }
  if (pathname.startsWith("/api/") && !authorized(req)) return new Response("not signed in; open the dashboard with `rekcod dashboard`", { status: 401 })
  const res = NextResponse.next()
  for (const c of expire) res.headers.append("set-cookie", c)
  return res
}

// Reads the token from its own URL (checked above), so the HTML doesn't echo it, then
// replaces the history entry so the token isn't left in the address bar or Back.
const AUTH_PAGE = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Signing in · rekcod</title></head><body>
<noscript>The dashboard needs JavaScript.</noscript>
<script>try{localStorage.setItem(${JSON.stringify(TOKEN_KEY)},new URLSearchParams(location.search).get("token"));location.replace("/")}catch(e){document.body.textContent="This browser didn't let the dashboard keep its sign-in in localStorage: "+e}</script>
</body></html>`

function signIn(why: string, expire: string[]) {
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Sign in · rekcod</title>
<style>:root{color-scheme:light dark}body{font:15px/1.5 system-ui,sans-serif;max-width:34rem;margin:15vh auto;padding:0 16px}code{font:13px ui-monospace,monospace;padding:2px 4px;border-radius:4px;background:#8882}</style></head>
<body><h1>Open this with <code>rekcod dashboard</code></h1><p>The dashboard runs commands as you, so it needs the sign-in link that <code>rekcod dashboard</code> opens (or <code>npm run dev</code> prints). Each launch has a new one.</p><p>${why}</p></body></html>`
  return page(401, html, expire)
}

// referrer-policy keeps the /auth URL, token included, out of any request the page makes.
function page(status: number, html: string, expire: string[]) {
  const headers = new Headers({
    "content-type": "text/html; charset=utf-8",
    "cache-control": "no-store",
    "referrer-policy": "no-referrer",
    "x-frame-options": "DENY",
    "content-security-policy": "frame-ancestors 'none'",
  })
  for (const c of expire) headers.append("set-cookie", c)
  return new Response(html, { status, headers })
}

// Static build files and icons carry nothing private; the browser fetches the manifest
// without credentials.
export const config = {
  matcher: ["/((?!_next/static|_next/image|icon\\.svg|apple-icon\\.png|manifest\\.webmanifest|favicon\\.ico).*)"],
}
