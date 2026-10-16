// `npm run bundle`: `next build`, then the static files the standalone server needs next to
// it (Next leaves them out). `npm run bundle -- --install` also copies the result to
// ~/.rekcod/dashboard (or $REKCOD_HOME/dashboard), where `rekcod dashboard` looks first.
import { spawnSync } from "node:child_process"
import { cpSync, existsSync, rmSync } from "node:fs"
import os from "node:os"
import path from "node:path"

const dist = process.env.NEXT_DIST_DIR || ".next"
const out = path.join(dist, "standalone")
const build = spawnSync("next", ["build"], { stdio: "inherit", env: process.env, shell: false })
if (build.status !== 0) process.exit(build.status ?? 1)
if (!existsSync(path.join(out, "server.js"))) {
  console.error(`bundle: no ${out}/server.js; is output: "standalone" set in next.config.ts?`)
  process.exit(1)
}
cpSync(path.join(dist, "static"), path.join(out, dist, "static"), { recursive: true })
if (existsSync("public")) cpSync("public", path.join(out, "public"), { recursive: true })
// The entry `rekcod dashboard` runs; it refuses a build without it.
cpSync("scripts/start.cjs", path.join(out, "start.cjs"))
console.log(`bundle: ${path.resolve(out)} (node start.cjs)`)

if (process.argv.includes("--install")) {
  const dest = path.join(process.env.REKCOD_HOME || path.join(os.homedir(), ".rekcod"), "dashboard")
  rmSync(dest, { recursive: true, force: true })
  cpSync(out, dest, { recursive: true })
  console.log(`bundle: installed to ${dest}; \`rekcod dashboard\` runs it`)
}
