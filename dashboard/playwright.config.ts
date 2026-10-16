import path from "node:path"
import { defineConfig, devices } from "@playwright/test"

// UI checks against a production build serving fixtures/state.json (every action is a dry run),
// in its own build dir and on its own port so a running `npm run dev` isn't touched.
const PORT = Number(process.env.E2E_PORT) || 4599

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: { baseURL: `http://127.0.0.1:${PORT}`, trace: "retain-on-failure" },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"] }, grepInvert: /@phone/ },
    { name: "phone", use: { ...devices["Desktop Chrome"], viewport: { width: 390, height: 844 } }, grep: /@phone/ },
  ],
  webServer: {
    // The same standalone bundle `rekcod dashboard` runs.
    command: `node scripts/bundle.mjs && node .next-e2e/standalone/server.js`,
    url: `http://127.0.0.1:${PORT}/api/state`,
    env: { REKCOD_STATE_FILE: path.resolve("fixtures/state.json"), NEXT_DIST_DIR: ".next-e2e", HOSTNAME: "127.0.0.1", PORT: String(PORT) },
    timeout: 180_000,
    reuseExistingServer: !process.env.CI,
  },
})
