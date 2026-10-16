import { defineConfig } from "vitest/config"
import path from "node:path"

// e2e/ is Playwright's (npm run test:e2e). "server-only" throws outside React's server
// build, so tests get its empty variant.
export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
      "server-only": path.resolve(__dirname, "node_modules/server-only/empty.js"),
    },
  },
  test: { include: ["src/**/*.test.ts"] },
})
