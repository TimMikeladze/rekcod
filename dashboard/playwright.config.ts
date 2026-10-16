import path from "node:path"
import { defineConfig, devices } from "@playwright/test"

// UI checks against a production build serving fixtures/state.json (every action is a dry run),
// in its own build dir and on its own port so a running `npm run dev` isn't touched.
const PORT = Number(process.env.E2E_PORT) || 4599
// Fixed so a reused server (reuseExistingServer) still takes it. The setup project signs in
// through /auth with it; workers inherit it from here.
process.env.E2E_TOKEN ||= "e2e0".repeat(16)
export const AUTH_STATE = "e2e/.auth/state.json"

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: { baseURL: `http://127.0.0.1:${PORT}`, trace: "retain-on-failure" },
  projects: [
    { name: "setup", testMatch: /auth\.setup\.ts/ },
    { name: "desktop", use: { ...devices["Desktop Chrome"], storageState: AUTH_STATE }, grepInvert: /@phone/, dependencies: ["setup"] },
    {
      name: "phone",
      use: { ...devices["Desktop Chrome"], viewport: { width: 390, height: 844 }, storageState: AUTH_STATE },
      grep: /@phone/,
      dependencies: ["setup"],
    },
  ],
  webServer: {
    // The same standalone bundle and entry `rekcod dashboard` runs. /api/state answers 401
    // until signed in, which Playwright counts as up.
    command: `node scripts/bundle.mjs && node .next-e2e/standalone/start.cjs`,
    url: `http://127.0.0.1:${PORT}/api/state`,
    env: {
      REKCOD_STATE_FILE: path.resolve("fixtures/state.json"),
      NEXT_DIST_DIR: ".next-e2e",
      PORT: String(PORT),
      REKCOD_DASHBOARD_TOKEN: process.env.E2E_TOKEN,
    },
    timeout: 180_000,
    reuseExistingServer: !process.env.CI,
  },
})
