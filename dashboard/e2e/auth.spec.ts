import { createServer, type IncomingHttpHeaders, type Server } from "node:http"
import type { AddressInfo } from "node:net"
import { expect, test } from "@playwright/test"

const TOKEN = process.env.E2E_TOKEN!
const SIGN_IN = { name: "Open this with rekcod dashboard" }

test.describe("signed out", () => {
  test.use({ storageState: { cookies: [], origins: [] } })

  test("the API answers 401 without the header, GETs included", async ({ request }) => {
    expect((await request.get("/api/state")).status()).toBe(401)
    expect((await request.get("/api/logs?name=shop-web-1")).status()).toBe(401)
    expect((await request.get("/api/state", { headers: { authorization: "Bearer nope" } })).status()).toBe(401)
  })

  test("the token in a cookie gets 401", async ({ request, baseURL }) => {
    const cookie = `rekcod-dashboard-${new URL(baseURL!).port}=${TOKEN}`
    expect((await request.get("/api/state", { headers: { cookie } })).status()).toBe(401)
    expect((await request.post("/api/run", { headers: { cookie, "x-rekcod": "1" }, data: { argv: ["ps"] } })).status()).toBe(401)
  })

  test("a page says how to open the dashboard and calls no API", async ({ page }) => {
    const calls: string[] = []
    page.on("request", (r) => new URL(r.url()).pathname.startsWith("/api/") && calls.push(r.url()))
    await page.goto("/containers")
    await expect(page.getByRole("heading", SIGN_IN)).toBeVisible()
    expect(calls).toEqual([])
  })

  test("a wrong token doesn't sign in", async ({ page }) => {
    const res = await page.goto("/auth?token=nope")
    expect(res?.status()).toBe(401)
    expect(await page.evaluate(() => localStorage.length)).toBe(0)
    expect(await page.context().cookies()).toEqual([])
  })

  test("/auth isn't cached, sends no referrer and doesn't echo the token", async ({ request }) => {
    const res = await request.get(`/auth?token=${TOKEN}`)
    expect(res.status()).toBe(200)
    expect(res.headers()["cache-control"]).toBe("no-store")
    expect(res.headers()["referrer-policy"]).toBe("no-referrer")
    expect(res.headers()["set-cookie"]).toBeUndefined()
    expect(await res.text()).not.toContain(TOKEN)
  })
})

test("signed in, the API answers to the header", async ({ request }) => {
  expect((await request.get("/api/state", { headers: { authorization: `Bearer ${TOKEN}` } })).status()).toBe(200)
})

test("the event stream, logs and the screenshot load with the header", async ({ page }) => {
  await page.goto("/")
  // "live" means /api/events is connected; without it the page polls.
  await expect(page.getByText("live", { exact: true })).toBeVisible()
  await page.goto("/sandboxes/m4c0s9zz")
  await expect(page.getByRole("img", { name: "Screen of sandbox ui-check" })).toBeVisible()
  expect(await page.getByRole("img", { name: "Screen of sandbox ui-check" }).evaluate((i: HTMLImageElement) => i.naturalWidth)).toBeGreaterThan(0)
  await page.goto("/containers/shop-web-1?tab=logs")
  await expect(page.getByText("GET /api/products 200").first()).toBeVisible()
})

test("a token from an earlier launch shows the sign-in page", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
  await page.evaluate(() => localStorage.setItem("rekcod.token", "0".repeat(64)))
  await page.reload()
  await expect(page.getByRole("heading", SIGN_IN)).toBeVisible()
  await expect(page.getByText("earlier launch")).toBeVisible()
})

// The audit's S1: cookies ignore ports, so a sign-in cookie on localhost went to every other
// localhost server, which could replay it.
test.describe("another localhost port", () => {
  test.use({ storageState: { cookies: [], origins: [] } })
  let other: Server
  const seen: IncomingHttpHeaders[] = []

  test.beforeAll(async () => {
    other = createServer((req, res) => {
      seen.push(req.headers)
      res.setHeader("content-type", "text/html")
      res.end(req.url === "/" ? `<!doctype html><title>other</title><script>fetch("/probe")</script>` : "")
    })
    await new Promise<void>((ok) => other.listen(0, "127.0.0.1", ok))
  })
  test.afterAll(() => new Promise<void>((ok) => other.close(() => ok())))

  test("receives no credential after signing in", async ({ page, baseURL }) => {
    const port = new URL(baseURL!).port
    const otherPort = (other.address() as AddressInfo).port
    // What an earlier build left behind; signing in deletes it.
    await page.context().addCookies([{ name: `rekcod-dashboard-${port}`, value: TOKEN, domain: "localhost", path: "/", httpOnly: true, sameSite: "Strict" }])

    await page.goto(`http://localhost:${port}/auth?token=${TOKEN}`)
    await expect(page).toHaveURL(`http://localhost:${port}/`)
    await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
    expect(await page.context().cookies()).toEqual([])

    await page.goto(`http://localhost:${otherPort}/`)
    await expect(page).toHaveTitle("other")
    await expect.poll(() => seen.length).toBeGreaterThanOrEqual(2)
    for (const h of seen) {
      expect(h.cookie ?? "").not.toContain(TOKEN)
      expect(h.authorization).toBeUndefined()
    }
    expect(await page.evaluate(() => localStorage.length)).toBe(0)
  })
})
