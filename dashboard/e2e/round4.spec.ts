import { readFileSync } from "node:fs"
import path from "node:path"
import { expect, test, type Page } from "@playwright/test"
import type { State } from "../src/lib/types"

// Sorting, bulk select, tabs, the log viewer, shortcuts, the palette, pins, density and the
// rest of docs/dashboard-round4.md. Same fixture server as dashboard.spec.ts.

const FIXTURE = JSON.parse(readFileSync(path.resolve("fixtures/state.json"), "utf8")) as State

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (e) => {
    throw e
  })
  page.on("console", (m) => {
    if (m.type() === "error" && /hydrat/i.test(m.text())) throw new Error(m.text())
  })
})

/** Serves `states` from /api/state in turn (the last one repeats) with the event stream down, so the page polls. */
async function serveStates(page: Page, states: State[]) {
  let i = 0
  await page.route("**/api/events", (r) => r.abort())
  await page.route("**/api/state", (r) => r.fulfill({ json: states[Math.min(i++, states.length - 1)] }))
}

const rowNames = (page: Page) => page.locator("[data-navrow]").evaluateAll((rows) => rows.map((r) => r.getAttribute("aria-label")))

test("a header click sorts the list and keeps it in the URL", async ({ page }) => {
  await page.goto("/images")
  await page.getByRole("button", { name: "Size" }).click()
  await expect(page).toHaveURL(/sort=size/)
  await page.getByRole("button", { name: "Size" }).click()
  await expect(page).toHaveURL(/sort=-size/)
  await expect(page.getByRole("columnheader", { name: "Size" })).toHaveAttribute("aria-sort", "descending")
  const names = await rowNames(page)
  expect(names[0]).toBe("Image shop-web:dev")
  await page.reload()
  // Rows render once the first state arrives; wait for them before reading the order.
  await expect(page.locator("[data-navrow]")).toHaveCount(names.length)
  expect(await rowNames(page)).toEqual(names)
})

test("bulk select runs each row's own action; cancelling the confirm runs nothing", async ({ page }) => {
  await page.goto("/containers")
  await page.getByLabel("Select shop-web-1").check()
  await page.getByLabel("Select redis").check()
  const bar = page.getByRole("region", { name: "Selected rows" })
  await expect(bar).toContainText("2 containers")
  await bar.getByRole("button", { name: "Remove" }).click()
  const dialog = page.getByRole("alertdialog")
  await expect(dialog).toContainText("rekcod rm -f shop-web-1")
  await expect(dialog).toContainText("rekcod rm -f redis")
  await dialog.getByRole("button", { name: "Cancel" }).click()
  // Still selected after a cancel.
  await expect(bar).toContainText("2 containers")
  await bar.getByRole("button", { name: "Restart" }).click()
  await expect(page.getByText("2 done")).toBeVisible()
  await expect(bar).toHaveCount(0)
  await page.goto("/activity?q=rekcod+rm")
  await expect(page.getByText(/rekcod rm -f (shop-web-1|redis)/)).toHaveCount(0)
})

test("x ticks the focused row and Escape clears the selection", async ({ page }) => {
  await page.goto("/volumes")
  await page.locator("[data-navrow]").first().focus()
  await page.keyboard.press("x")
  await expect(page.getByRole("region", { name: "Selected rows" })).toContainText("1 volume")
  await page.keyboard.press("Escape")
  await expect(page.getByRole("region", { name: "Selected rows" })).toHaveCount(0)
})

test("container tabs: logs, inspect and env each have their own URL", async ({ page }) => {
  await page.goto("/containers/shop-web-1")
  const tabs = page.getByRole("navigation", { name: "Container views" })
  await tabs.getByRole("link", { name: "Logs" }).click()
  await expect(page).toHaveURL(/tab=logs/)
  await expect(page.getByRole("log", { name: "Log output" })).toContainText("GET /health")
  await tabs.getByRole("link", { name: "Inspect" }).click()
  await expect(page.getByLabel("docker inspect output")).toContainText('"RestartCount"')
  await expect(page.getByLabel("docker inspect output")).not.toContainText("hunter2")
  await tabs.getByRole("link", { name: /Env/ }).click()
  await expect(page.getByRole("button", { name: "Show DATABASE_URL" })).toBeVisible()
})

test("log search: regex, match count, a bad regex, next error, only matching", async ({ page }) => {
  await page.goto("/containers/shop-web-1?tab=logs")
  const log = page.getByRole("log", { name: "Log output" })
  await expect(log).toContainText("GET /health")
  await page.getByRole("button", { name: "Regular expression" }).click()
  const search = page.getByLabel("Search the log")
  await search.fill("(")
  await expect(page.getByText("bad regex")).toBeVisible()
  await search.fill("4\\d\\d \\d+ms")
  await expect(page.getByText(/\d+ match/)).toBeVisible()
  await search.press("Enter")
  await expect(page.getByText(/^1\/\d+ match/)).toBeVisible()
  await page.getByRole("button", { name: "Only matching" }).click()
  expect(await log.locator("[data-line]").allTextContents()).toEqual(expect.arrayContaining([expect.stringContaining("404")]))
  for (const t of await log.locator("[data-line]").allTextContents()) expect(t).toMatch(/4\d\d \d+ms/)
  await page.getByRole("button", { name: /Next error/ }).click()
  await expect(log.locator(".bg-ok-bg")).toContainText("error")
})

test("only the log lines near the view are rendered, wrapped or not", async ({ page }) => {
  await page.goto("/containers/shop-web-1?tab=logs")
  const log = page.getByRole("log", { name: "Log output" })
  await expect(log).toContainText("GET /health")
  // The fixture streams 200 lines and keeps adding; the view holds about 30.
  await expect.poll(() => log.locator("[data-line]").count()).toBeLessThan(150)
  await page.getByRole("button", { name: "Wrap" }).click()
  await expect.poll(() => log.locator("[data-line]").count()).toBeLessThan(150)
  // Scrolled to the top, the first line is there.
  await log.evaluate((el) => (el.scrollTop = 0))
  await expect(log.locator('[data-line="0"]')).toBeVisible()
})

test("? opens the shortcut sheet and g a goes to apps", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
  await page.keyboard.press("?")
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toContainText("g a")
  await page.keyboard.press("Escape")
  await expect(page.getByRole("dialog")).toHaveCount(0)
  await page.keyboard.press("g")
  await page.keyboard.press("a")
  await expect(page).toHaveURL(/\/apps$/)
})

test("Cmd+K goes to pages and lists recent picks first", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
  await page.keyboard.press("ControlOrMeta+k")
  await page.getByRole("combobox", { name: "Search rows and actions" }).fill("disk")
  await expect(page.getByRole("option").first()).toContainText("Disk")
  await page.keyboard.press("Enter")
  await expect(page).toHaveURL(/\/disk$/)
  await page.keyboard.press("ControlOrMeta+k")
  await expect(page.getByRole("group", { name: "Recent" }).getByRole("option").first()).toContainText("Disk")
  await page.getByRole("combobox", { name: "Search rows and actions" }).fill("remove old-worker")
  await expect(page.getByRole("option").first()).toContainText("Remove container old-worker?")
})

test("empty lists without a remote show the commands to add one", async ({ page }) => {
  await serveStates(page, [{ ...FIXTURE, remote: null, apps: [], jobs: [], nodes: null }])
  await page.goto("/apps")
  await expect(page.getByText("No fleet remote")).toBeVisible()
  await expect(page.getByText("rekcod remote add team http://controller:7700 --token ADMIN_TOKEN")).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy: remote add" })).toBeVisible()
})

test("the success toast's View opens the run in Activity", async ({ page }) => {
  await page.goto("/containers")
  await page.getByRole("button", { name: "Restart shop-web-1" }).click()
  await page.getByRole("button", { name: "View" }).click()
  await expect(page).toHaveURL(/\/activity\?run=\d+/)
  await expect(page.locator("li [aria-expanded=true]")).toContainText("rekcod restart shop-web-1")
})

test("a row flashes when its state changes", async ({ page }) => {
  const stopped = structuredClone(FIXTURE)
  stopped.containers.find((c) => c.name === "redis")!.state = "exited"
  await serveStates(page, [FIXTURE, stopped])
  await page.goto("/containers")
  await expect(page.locator('[data-navrow][aria-label="redis, exited"][data-flash]')).toBeVisible({ timeout: 8000 })
})

test("activity filters, and Run again only while the argv is offered", async ({ page }) => {
  await page.goto("/containers")
  await page.getByRole("button", { name: "Restart redis" }).click()
  await expect(page.getByText(/dry run: rekcod restart redis/)).toBeVisible()
  await page.goto("/activity?subject=redis")
  await expect(page.getByLabel("Subject")).toHaveValue("redis")
  await expect(page.getByRole("button", { name: "Run again: rekcod restart redis" }).first()).toBeVisible()
  await page.getByRole("button", { name: "Failed only" }).click()
  await expect(page).toHaveURL(/failed=1/)
  await expect(page.getByText("No run matches these filters.")).toBeVisible()
  // A state where redis is gone: its restart isn't offered, so there's nothing to run again.
  await serveStates(page, [{ ...FIXTURE, containers: FIXTURE.containers.filter((c) => c.name !== "redis") }])
  await page.goto("/activity?subject=redis")
  await expect(page.getByText("rekcod restart redis").first()).toBeVisible()
  await expect(page.getByRole("button", { name: /Run again/ })).toHaveCount(0)
})

test("charts: overview sparklines, app traffic, the jobs timeline, node history", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByRole("img", { name: /Total CPU use of all containers/ })).toBeVisible()
  await page.goto("/apps")
  await expect(page.getByRole("img", { name: "Requests per minute to dash over the last hour" })).toBeVisible()
  await expect(page.getByRole("columnheader", { name: "5xx" })).toBeVisible()
  await page.goto("/jobs")
  await expect(page.getByRole("img", { name: /Job runs in the last 24 hours/ })).toBeVisible()
  await page.goto("/nodes/spare")
  await expect(page.getByRole("img", { name: "load of node spare over the last hour" })).toBeVisible()
})

test("fleet views are tabs in the URL", async ({ page }) => {
  await page.goto("/fleet")
  await page.getByRole("navigation", { name: "Fleet views" }).getByRole("link", { name: "Remotes and hosts" }).click()
  await expect(page).toHaveURL(/view=remotes/)
  await expect(page.getByRole("heading", { name: "Docker hosts" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Nodes" })).toHaveCount(0)
})

test("disk shows the breakdown, what pruning frees and the largest images", async ({ page }) => {
  await page.goto("/disk")
  await expect(page.getByRole("heading", { name: "Reclaimable" })).toBeVisible()
  await page.getByRole("link", { name: /shop-web:dev/ }).click()
  await expect(page).toHaveURL(/\/images\?q=shop-web/)
})

test("a pinned container sits in the sidebar after a reload", async ({ page }) => {
  await page.goto("/containers/redis")
  await page.getByRole("button", { name: "Pin" }).click()
  const pinned = page.getByRole("group", { name: "Pinned" })
  await expect(pinned.getByRole("link", { name: /redis/ })).toBeVisible()
  await page.reload()
  await expect(pinned.getByRole("link", { name: /redis/ })).toBeVisible()
  await page.getByRole("button", { name: "Unpin" }).click()
  await expect(pinned).toHaveCount(0)
})

test("compact tables stay compact after a reload", async ({ page }) => {
  await page.goto("/containers")
  await page.getByRole("radio", { name: "Compact" }).click()
  await expect(page.locator("html")).toHaveAttribute("data-density", "compact")
  await page.reload()
  await expect(page.locator("html")).toHaveAttribute("data-density", "compact")
  await page.getByRole("radio", { name: "Comfortable" }).click()
  await expect(page.locator("html")).not.toHaveAttribute("data-density", "compact")
})

test("the tab title counts problems and the icon is the rekcod mark", async ({ page }) => {
  await page.goto("/apps")
  await expect(page).toHaveTitle(/^\(\d+\) Apps · rekcod$/)
  await expect(page.locator('link[rel="icon"][href*="icon.svg"]')).toHaveCount(1)
  await expect(page.locator('link[rel="apple-touch-icon"][href*="apple-icon.png"]')).toHaveCount(1)
})
