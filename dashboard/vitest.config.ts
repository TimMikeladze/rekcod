import { defineConfig } from "vitest/config"
import path from "node:path"

// e2e/ is Playwright's (npm run test:e2e).
export default defineConfig({
  resolve: { alias: { "@": path.resolve(__dirname, "src") } },
  test: { include: ["src/**/*.test.ts"] },
})
