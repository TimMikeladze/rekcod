import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // A second copy (e.g. one serving fixtures/state.json) needs its own build dir.
  distDir: process.env.NEXT_DIST_DIR || ".next",
  // `rekcod dashboard` runs .next/standalone/server.js (npm run bundle). Tracing from this
  // directory keeps server.js at the top of standalone/ instead of under the repo's path.
  output: "standalone",
  outputFileTracingRoot: process.cwd(),
  cacheComponents: true,
  partialPrefetching: true,
  turbopack: {
    rules: {
      "*.css": {
        loaders: ["@tailwindcss/turbopack"],
        as: "*.css",
      },
    },
  },
};

export default nextConfig;
