import { expect, test as setup } from "@playwright/test"
import { AUTH_STATE } from "../playwright.config"

// Signs in the way `rekcod dashboard` does, and saves the cookie for the other projects.
setup("sign in through /auth", async ({ page }) => {
  await page.goto(`/auth?token=${process.env.E2E_TOKEN}`)
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible()
  await page.context().storageState({ path: AUTH_STATE })
})
