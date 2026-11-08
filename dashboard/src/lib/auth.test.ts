import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { api, authStatus, subscribeAuth, TOKEN_KEY } from "./api"
import { authorized, expireLegacyCookies, sameSecret, serverToken, TOKEN_ENV } from "./auth"
import { refuse } from "./guard"

const TOKEN = "a".repeat(64)
const HOST = "localhost:4500"

function req(headers: Record<string, string>, method = "GET") {
  return new Request("http://localhost:4500/api/state", { method, headers: { host: HOST, ...headers } })
}
const signedIn = (h: Record<string, string> = {}, method = "GET") => req({ authorization: `Bearer ${TOKEN}`, ...h }, method)

beforeEach(() => {
  process.env[TOKEN_ENV] = TOKEN
})
afterEach(() => {
  delete process.env[TOKEN_ENV]
})

describe("token", () => {
  it("compares secrets of any length without throwing", () => {
    expect(sameSecret(TOKEN, TOKEN)).toBe(true)
    expect(sameSecret("b".repeat(64), TOKEN)).toBe(false)
    expect(sameSecret("a", TOKEN)).toBe(false)
    expect(sameSecret("", TOKEN)).toBe(false)
  })
  it("accepts the right bearer header only", () => {
    expect(authorized(signedIn())).toBe(true)
    expect(authorized(req({ authorization: `bearer ${TOKEN}` }))).toBe(true)
    expect(authorized(req({ authorization: `Bearer ${"b".repeat(64)}` }))).toBe(false)
    expect(authorized(req({ authorization: TOKEN }))).toBe(false)
    expect(authorized(req({ authorization: `Basic ${TOKEN}` }))).toBe(false)
    expect(authorized(req({}))).toBe(false)
  })
  it("ignores the token in a cookie, which any localhost server could replay", () => {
    expect(authorized(req({ cookie: `rekcod-dashboard-4500=${TOKEN}` }))).toBe(false)
    expect(authorized(req({ cookie: `rekcod-dashboard-4500=${TOKEN}`, authorization: "Bearer " }))).toBe(false)
  })
  it("refuses everyone when the server has no token or a short one", () => {
    delete process.env[TOKEN_ENV]
    expect(serverToken()).toBeNull()
    expect(authorized(signedIn())).toBe(false)
    process.env[TOKEN_ENV] = "short"
    expect(authorized(req({ authorization: "Bearer short" }))).toBe(false)
  })
  it("expires every old sign-in cookie the browser sent, whatever its port", () => {
    expect(expireLegacyCookies(`theme=dark; rekcod-dashboard-4500=${TOKEN}; rekcod-dashboard-4501=x; bare`)).toEqual([
      "rekcod-dashboard-4500=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict",
      "rekcod-dashboard-4501=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict",
    ])
    expect(expireLegacyCookies(null)).toEqual([])
    expect(expireLegacyCookies("rekcod-dashboard-x=1")).toEqual([])
  })
})

describe("refuse", () => {
  it("lets a signed-in same-origin request through", () => {
    expect(refuse(signedIn(), false)).toBeNull()
    expect(refuse(signedIn({ origin: "http://localhost:4500", "sec-fetch-site": "same-origin", "x-rekcod": "1" }, "POST"), true)).toBeNull()
  })
  it("checks Host before anything, so a rebinding page gets 403 even with the token", () => {
    expect(refuse(signedIn({ host: "evil.example:4500" }), false)?.status).toBe(403)
    expect(refuse(req({ host: "evil.example" }), false)?.status).toBe(403)
  })
  it("401s without the header, GETs included, and with the token only in a cookie", () => {
    expect(refuse(req({}), false)?.status).toBe(401)
    expect(refuse(req({ "x-rekcod": "1" }, "POST"), true)?.status).toBe(401)
    expect(refuse(req({ cookie: `rekcod-dashboard-4500=${TOKEN}` }), false)?.status).toBe(401)
  })
  it("refuses other origins, the null origin and other sites", () => {
    expect(refuse(signedIn({ origin: "http://localhost:3000" }), false)?.status).toBe(403)
    expect(refuse(signedIn({ origin: "null" }), false)?.status).toBe(403)
    expect(refuse(signedIn({ origin: "not a url" }), false)?.status).toBe(403)
    expect(refuse(signedIn({ "sec-fetch-site": "same-site" }), false)?.status).toBe(403)
    expect(refuse(signedIn({ "sec-fetch-site": "cross-site" }), false)?.status).toBe(403)
  })
  it("needs x-rekcod on mutating requests", () => {
    expect(refuse(signedIn({}, "POST"), true)?.status).toBe(403)
    expect(refuse(signedIn({ "x-rekcod": "0" }, "POST"), true)?.status).toBe(403)
  })
})

describe("api()", () => {
  const store = new Map<string, string>()
  const fetchMock = vi.fn<typeof fetch>()
  beforeEach(() => {
    store.clear()
    fetchMock.mockReset()
    vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => store.set(k, v) })
    vi.stubGlobal("window", { addEventListener: () => {}, removeEventListener: () => {} })
    vi.stubGlobal("fetch", fetchMock)
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })
  const sentAuth = (i: number) => new Headers(fetchMock.mock.calls[i][1]?.headers).get("authorization")

  it("sends nothing without a token", async () => {
    expect(authStatus()).toBe("out")
    expect((await api("/api/state")).status).toBe(401)
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("adds the bearer header, and after a 401 stops until a new token is stored", async () => {
    const onChange = vi.fn()
    const unsub = subscribeAuth(onChange)
    store.set(TOKEN_KEY, "t1")
    fetchMock.mockResolvedValueOnce(new Response("{}", { status: 200 }))
    await api("/api/run", { method: "POST", headers: { "x-rekcod": "1" } })
    expect(sentAuth(0)).toBe("Bearer t1")
    expect(new Headers(fetchMock.mock.calls[0][1]?.headers).get("x-rekcod")).toBe("1")
    expect(fetchMock.mock.calls[0][1]?.cache).toBe("no-store")

    // The server restarted with a new token.
    fetchMock.mockResolvedValueOnce(new Response("", { status: 401 }))
    await api("/api/state")
    expect(authStatus()).toBe("stale")
    expect(onChange).toHaveBeenCalledTimes(1)
    expect((await api("/api/state")).status).toBe(401)
    expect(fetchMock).toHaveBeenCalledTimes(2)
    // Kept: another tab may have stored it.
    expect(store.get(TOKEN_KEY)).toBe("t1")

    store.set(TOKEN_KEY, "t2")
    expect(authStatus()).toBe("in")
    fetchMock.mockResolvedValueOnce(new Response("{}", { status: 200 }))
    await api("/api/state")
    expect(sentAuth(2)).toBe("Bearer t2")
    unsub()
  })
})
