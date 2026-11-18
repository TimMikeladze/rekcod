import type { NextConfig } from "next";

// Scripts and connections from this origin only, and nothing that frames it. Inline scripts
// stay allowed: Next streams its own (`self.__next_f.push`) into every page, and a nonce
// would need every page rendered per request, which cacheComponents and partial prefetching
// don't do. Dev needs eval for React's error overlays.
const CSP = [
  "default-src 'self'",
  `script-src 'self' 'unsafe-inline'${process.env.NODE_ENV === "development" ? " 'unsafe-eval'" : ""}`,
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' blob: data:",
  "font-src 'self'",
  "connect-src 'self'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'self'",
  "frame-ancestors 'none'",
].join("; ");

const nextConfig: NextConfig = {
  // A second copy (e.g. one serving fixtures/state.json) needs its own build dir.
  distDir: process.env.NEXT_DIST_DIR || ".next",
  // `rekcod dashboard` runs .next/standalone/start.cjs, which loads server.js (npm run bundle). Tracing from this
  // directory keeps server.js at the top of standalone/ instead of under the repo's path.
  output: "standalone",
  outputFileTracingRoot: process.cwd(),
  // A page on another port can't frame the dashboard and click its buttons.
  async headers() {
    return [{ source: "/:path*", headers: [{ key: "X-Frame-Options", value: "DENY" }, { key: "Content-Security-Policy", value: CSP }] }]
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
