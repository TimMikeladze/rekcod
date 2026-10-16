import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // A second copy (e.g. one serving fixtures/state.json) needs its own build dir.
  distDir: process.env.NEXT_DIST_DIR || ".next",
  // `rekcod dashboard` runs .next/standalone/start.cjs, which loads server.js (npm run bundle). Tracing from this
  // directory keeps server.js at the top of standalone/ instead of under the repo's path.
  output: "standalone",
  outputFileTracingRoot: process.cwd(),
  // Another localhost port is the same site, so its pages get the SameSite=Strict cookie;
  // they still can't frame the dashboard and click its buttons.
  async headers() {
    return [{ source: "/:path*", headers: [{ key: "X-Frame-Options", value: "DENY" }, { key: "Content-Security-Policy", value: "frame-ancestors 'none'" }] }]
  },
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
