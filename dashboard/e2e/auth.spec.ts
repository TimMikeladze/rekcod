import { expect, test } from "@playwright/test"

test.describe("without the cookie", () => {
  test.use({ storageState: { cookies: [], origins: [] } })

  test("the API answers 401, GETs included", async ({ request }) => {
    expect((await request.get("/api/state")).status()).toBe(401)
    expect((await request.get("/api/logs?name=shop-web-1")).status()).toBe(401)
  })

  test("a page says how to open the dashboard", async ({ page }) => {
    const res = await page.goto("/containers")
    expect(res?.status()).toBe(401)
    await expect(page.getByRole("heading", { name: "Open this with rekcod dashboard" })).toBeVisible()
  })

  test("a wrong token doesn't sign in", async ({ page }) => {
    const res = await page.goto("/auth?token=nope")
    expect(res?.status()).toBe(401)
    expect(await page.context().cookies()).toEqual([])
  })
})

test("signed in, the API answers", async ({ request }) => {
  expect((await request.get("/api/state")).status()).toBe(200)
})
