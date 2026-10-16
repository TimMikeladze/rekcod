import { EventEmitter } from "node:events"
import type { ChildProcess } from "node:child_process"
import { afterEach, beforeEach, describe, expect, it } from "vitest"
import { authCookie, authorized, cookieName, sameSecret, serverToken, TOKEN_ENV } from "./auth"
import { refuse } from "./guard"
import { cappedStream, MAX_STREAMS, openStreams } from "./server/stream"

const TOKEN = "a".repeat(64)
const HOST = "localhost:4500"

function req(headers: Record<string, string>, method = "GET") {
  return new Request("http://localhost:4500/api/state", { method, headers: { host: HOST, ...headers } })
}
const signedIn = (h: Record<string, string> = {}, method = "GET") => req({ cookie: `${cookieName(HOST)}=${TOKEN}`, ...h }, method)

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
  it("accepts the right cookie only", () => {
    expect(authorized(signedIn())).toBe(true)
    expect(authorized(req({ cookie: `${cookieName(HOST)}=${"b".repeat(64)}` }))).toBe(false)
    expect(authorized(req({}))).toBe(false)
    // Another port's cookie, which the browser also sends to this one.
    expect(authorized(req({ cookie: `rekcod-dashboard-4501=${TOKEN}` }))).toBe(false)
  })
  it("refuses everyone when the server has no token or a short one", () => {
    delete process.env[TOKEN_ENV]
    expect(serverToken()).toBeNull()
    expect(authorized(signedIn())).toBe(false)
    process.env[TOKEN_ENV] = "short"
    expect(authorized(req({ cookie: `${cookieName(HOST)}=short` }))).toBe(false)
  })
  it("sets a strict, HttpOnly cookie named for the port", () => {
    expect(authCookie(HOST, TOKEN)).toBe(`rekcod-dashboard-4500=${TOKEN}; Path=/; HttpOnly; SameSite=Strict`)
    expect(cookieName("localhost")).toBe("rekcod-dashboard-80")
  })
})

describe("refuse", () => {
  it("lets a signed-in same-origin request through", () => {
    expect(refuse(signedIn(), false)).toBeNull()
    expect(refuse(signedIn({ origin: "http://localhost:4500", "sec-fetch-site": "same-origin", "x-rekcod": "1" }, "POST"), true)).toBeNull()
  })
  it("checks Host before anything, so a rebinding page gets 403 even with the cookie", () => {
    expect(refuse(signedIn({ host: "evil.example:4500" }), false)?.status).toBe(403)
    expect(refuse(req({ host: "evil.example" }), false)?.status).toBe(403)
  })
  it("401s without the cookie, GETs included", () => {
    expect(refuse(req({}), false)?.status).toBe(401)
    expect(refuse(req({ "x-rekcod": "1" }, "POST"), true)?.status).toBe(401)
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

describe("stream cap", () => {
  function fakeChild() {
    const c = new EventEmitter() as EventEmitter & { stdout: EventEmitter; stderr: EventEmitter; kill: () => void }
    c.stdout = new EventEmitter()
    c.stderr = new EventEmitter()
    c.kill = () => c.emit("close", null)
    return c as unknown as ChildProcess
  }

  it(`spawns up to ${MAX_STREAMS}, then 429s without spawning until one closes`, () => {
    const kids: ChildProcess[] = []
    const start = () => {
      const c = fakeChild()
      kids.push(c)
      return c
    }
    for (let i = 0; i < MAX_STREAMS; i++) expect(cappedStream(req({}), start).status).toBe(200)
    expect(openStreams()).toBe(MAX_STREAMS)
    expect(cappedStream(req({}), start).status).toBe(429)
    expect(kids).toHaveLength(MAX_STREAMS)

    // close and error on the same child count once.
    kids[0].emit("error", new Error("spawn failed"))
    kids[0].emit("close", 1)
    expect(openStreams()).toBe(MAX_STREAMS - 1)
    expect(cappedStream(req({}), start).status).toBe(200)
    for (const k of kids) k.emit("close", 0)
    expect(openStreams()).toBe(0)
  })
})
