import "server-only"
import { execFile, spawn } from "node:child_process"
import { existsSync } from "node:fs"
import path from "node:path"
import { findOffered } from "./allow"
import { staleBinary, staleMessage } from "./server/merge"
import type { Action, State } from "./types"

// REKCOD_BIN picks the binary (`rekcod dashboard` sets it to itself); REKCOD_STATE_FILE serves
// a saved `rekcod state` instead (UI work without a VM or a fleet).
export const BIN = process.env.REKCOD_BIN || findBin()
export const FIXTURE = process.env.REKCOD_STATE_FILE || ""

/** `rekcod` on PATH, else this checkout's release or debug build. */
function findBin(): string {
  const dirs = (process.env.PATH ?? "").split(":").filter(Boolean)
  const repo = path.resolve(process.cwd(), "..", "target")
  for (const p of [...dirs.map((d) => path.join(d, "rekcod")), path.join(repo, "release/rekcod"), path.join(repo, "debug/rekcod")]) {
    if (existsSync(p)) return p
  }
  return "rekcod"
}

export function run(argv: string[], opts: { cwd?: string; timeout?: number } = {}) {
  return new Promise<{ code: number; stdout: string; stderr: string }>((resolve) => {
    execFile(
      /*turbopackIgnore: true*/ BIN,
      argv,
      { cwd: opts.cwd, timeout: opts.timeout ?? 120_000, maxBuffer: 64 << 20 },
      (err, stdout, stderr) => {
        const code = err ? (typeof err.code === "number" ? err.code : 1) : 0
        resolve({ code, stdout, stderr: stderr || (err && !stderr ? String(err.message) : "") })
      },
    )
  })
}

/** `rekcod state` (`--local`: no controller calls), or the fixture file. */
export async function readState(local = false): Promise<State> {
  if (FIXTURE) {
    const { readFile } = await import("node:fs/promises")
    return JSON.parse(await readFile(FIXTURE, "utf8"))
  }
  const r = await run(local ? ["state", "--local"] : ["state"], { timeout: 30_000 })
  if (r.code !== 0) {
    if (staleBinary(r.stderr)) throw new Error(staleMessage(BIN))
    throw new Error(`${BIN} state: ${r.stderr.trim() || `exited ${r.code}`}`)
  }
  return JSON.parse(r.stdout)
}

/** A fresh full state; the allow-list checks use this, never the cache. */
export const state = () => readState(false)

/** The offered action with exactly this argv, or null. Only these run. */
export async function offered(argv: unknown): Promise<Action | null> {
  return findOffered(await state(), argv)
}

/** Opens Terminal.app running the action's command line, as the menu bar does. */
export function inTerminal(a: Action) {
  const q = (s: string) => (/^[A-Za-z0-9_\-./:=@+,]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`)
  let line = [BIN, ...(a.argv ?? [])].map(q).join(" ")
  if (a.dir) line = `cd ${q(a.dir)} && ${line}`
  const escaped = line.replace(/\\/g, "\\\\").replace(/"/g, '\\"')
  spawn(/*turbopackIgnore: true*/ "/usr/bin/osascript", ["-e", `tell application "Terminal"\nactivate\ndo script "${escaped}"\nend tell`], {
    stdio: "ignore",
    detached: true,
  }).unref()
}

export { STREAMED } from "./kinds"

export function spawnStream(a: Action) {
  return spawnRekcod(a.argv ?? [], a.dir)
}

export function spawnRekcod(argv: string[], cwd?: string) {
  return spawn(/*turbopackIgnore: true*/ BIN, argv, { cwd, stdio: ["ignore", "pipe", "pipe"] })
}
