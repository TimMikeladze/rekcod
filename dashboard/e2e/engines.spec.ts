import { expect, test } from "@playwright/test"

// The engine is one per server, so this project runs after the others (playwright.config.ts)
// and switches back when it's done.

test.afterAll(async ({ browser }) => {
  const page = await browser.newPage({ storageState: "e2e/.auth/state.json" })
  await page.goto("/")
  await page.getByRole("button", { name: /^Engine studio,/ }).click()
  await page.getByRole("menuitemradio", { name: "This Mac" }).click()
  await expect(page.getByRole("button", { name: /rekcod VM, running/ })).toBeVisible()
  await page.close()
})

test("another Mac's ports aren't read or linked on this one", async ({ page }) => {
  await page.goto("/containers")
  await page.getByRole("button", { name: /rekcod VM, running/ }).click()
  await page.getByRole("menuitemradio", { name: "studio" }).click()
  await expect(page.getByRole("button", { name: /^Engine studio, running/ })).toBeVisible()
  await expect(page.getByRole("link", { name: "shop-web-1" }).or(page.getByText("shop-web-1").first())).toBeVisible()
  await expect(page.locator('a[href^="http://localhost"]')).toHaveCount(0)
  await page.goto("/ports")
  await expect(page.getByText("studio:3000")).toBeVisible()
  await expect(page.getByText(/^HTTP \d+$/)).toHaveCount(0)
  await expect(page.getByText("localhost:3000")).toHaveCount(0)
})
