import AxeBuilder from "@axe-core/playwright"
import { expect, test, type Page } from "@playwright/test"

// Runs against fixtures/state.json (playwright.config.ts): every action is a dry run.

const TABS = ["containers", "images", "volumes", "apps", "jobs", "sandboxes", "projects", "ports", "fleet"]

async function noSeriousA11yIssues(page: Page) {
  const r = await new AxeBuilder({ page }).analyze()
  const bad = r.violations.filter((v) => v.impact === "serious" || v.impact === "critical")
  expect(bad.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).slice(0, 3).join(", ")}`)).toEqual([])
}

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (e) => {
    throw e
  })
  // A server/client markup mismatch only logs; make it fail the test.
  page.on("console", (m) => {
    if (m.type() === "error" && /hydrat/i.test(m.text())) throw new Error(m.text())
  })
})

for (const tab of TABS) {
  test(`/${tab} renders with no serious axe violations`, async ({ page }) => {
    await page.goto(`/${tab}`)
    await expect(page.getByRole("heading", { level: 1, name: new RegExp(tab, "i") })).toBeVisible()
    await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: new RegExp(tab, "i") })).toHaveAttribute("aria-current", "page")
    await expect(page).toHaveTitle(new RegExp(`^(\\(\\d+\\) )?${tab} · rekcod$`, "i"))
    await expect(page.getByText(/chart failed|Application error/)).toHaveCount(0)
    await noSeriousA11yIssues(page)
  })
}

test("old /?tab= links land on the list page", async ({ page }) => {
  await page.goto("/?tab=images&q=redis")
  await expect(page).toHaveURL(/\/images\?q=redis$/)
  await expect(page.getByLabel("Filter rows")).toHaveValue("redis")
})

test("the overview renders with no serious axe violations", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
  await expect(page.getByRole("heading", { name: /Needs attention/ })).toBeVisible()
  await noSeriousA11yIssues(page)
})

for (const path of ["/containers/shop-web-1", "/apps/dash", "/jobs/j-8f2k1m", "/nodes/studio", "/sandboxes/m4c0s9zz", "/disk", "/vm", "/activity"]) {
  test(`${path} renders with no serious axe violations`, async ({ page }) => {
    await page.goto(path)
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible()
    await noSeriousA11yIssues(page)
  })
}

test("the filter narrows the rows", async ({ page }) => {
  await page.goto("/containers")
  const rows = page.locator("[data-navrow]")
  await expect(rows.first()).toBeVisible()
  const before = await rows.count()
  await page.getByLabel("Filter rows").fill("redis")
  await expect(rows).toHaveCount(1)
  expect(before).toBeGreaterThan(1)
})

test("a confirm dialog opens and cancels without running anything", async ({ page }) => {
  await page.goto("/containers")
  await page.getByRole("button", { name: "More actions for old-worker" }).click()
  await page.getByRole("menuitem", { name: "Remove" }).click()
  const dialog = page.getByRole("alertdialog")
  await expect(dialog).toContainText("Remove container old-worker?")
  await dialog.getByRole("button", { name: "Cancel" }).click()
  await expect(dialog).toHaveCount(0)
  // Other tests share the server's activity log, so look for this remove only.
  await page.goto("/activity")
  await expect(page.getByRole("heading", { level: 1, name: "Activity" })).toBeVisible()
  await expect(page.getByText(/rekcod rm .*old-worker/)).toHaveCount(0)
})

test("a dry-run action shows its toast and lands in activity", async ({ page }) => {
  await page.goto("/containers")
  await page.getByRole("button", { name: "Restart shop-web-1" }).click()
  await expect(page.getByText(/dry run: rekcod restart shop-web-1/)).toBeVisible()
  await page.goto("/activity")
  await expect(page.getByText("rekcod restart shop-web-1")).toBeVisible()
})

test("logs stream on their own page", async ({ page }) => {
  await page.goto("/apps/dash")
  await page.getByRole("button", { name: "Logs dash" }).click()
  await expect(page).toHaveURL(/\/stream\?/)
  await expect(page.getByText(/dry run: rekcod app logs dash/)).toBeVisible()
})

test("keyboard: arrows move between rows, Enter opens one", async ({ page }) => {
  await page.goto("/apps")
  const first = page.locator("[data-navrow]").first()
  await first.focus()
  await page.keyboard.press("ArrowDown")
  await expect(page.locator("[data-navrow]").nth(1)).toBeFocused()
  await page.keyboard.press("ArrowUp")
  await page.keyboard.press("Enter")
  await expect(page).toHaveURL(/\/apps\/dash/)
})

test("Cmd+K finds an action", async ({ page }) => {
  await page.goto("/")
  // The palette mounts with the first state.
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
  await page.keyboard.press("ControlOrMeta+k")
  await page.getByRole("combobox", { name: "Search rows and actions" }).fill("restart web")
  await expect(page.getByRole("option").first()).toContainText("Restart shop-web-1")
})

test("@phone no sideways scroll at 390 px", async ({ page }) => {
  for (const path of ["/", "/apps", "/fleet", "/apps/dash", "/containers/shop-web-1", "/disk"]) {
    await page.goto(path)
    await expect(page.getByRole("heading", { level: 1 })).toBeAttached()
    const over = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
    expect(over, path).toBeLessThanOrEqual(0)
  }
})

test("@phone the sidebar opens as a drawer", async ({ page }) => {
  await page.goto("/")
  const nav = page.getByRole("navigation", { name: "Main" })
  await expect(nav).toBeHidden()
  await page.getByRole("button", { name: "Open navigation" }).click()
  await nav.getByRole("link", { name: /Apps/ }).click()
  await expect(page).toHaveURL(/\/apps$/)
  await expect(nav).toBeHidden()
})

test("Cmd+B folds the sidebar into icons and it stays folded after a reload", async ({ page }) => {
  await page.goto("/apps")
  const label = page.getByRole("navigation", { name: "Main" }).getByText("Containers", { exact: true })
  await expect(label).toBeVisible()
  await page.keyboard.press("ControlOrMeta+b")
  await expect(label).toBeHidden()
  await page.reload()
  await expect(page.getByRole("button", { name: "Expand sidebar" })).toBeVisible()
  await expect(label).toBeHidden()
  await page.getByRole("button", { name: "Expand sidebar" }).click()
  await expect(label).toBeVisible()
})

test("an unknown path shows not found", async ({ page }) => {
  const r = await page.goto("/nope")
  expect(r?.status()).toBe(404)
  await expect(page.getByRole("heading", { level: 1, name: "Not found" })).toBeVisible()
})
