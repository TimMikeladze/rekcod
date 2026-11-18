import "server-only"
import { execFile, spawn } from "node:child_process"
import { existsSync } from "node:fs"
import path from "node:path"
import { findOffered } from "./allow"
import { engine } from "./server/engine"
import { fakeEngine } from "./server/fixtures"
import { staleBinary, staleMessage } from "./server/merge"
import { runGroup } from "./server/proc"
import { appleScript, terminalScript, writeScript } from "./server/terminal"
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

export function run(argv: string[], opts: { cwd?: string; timeout?: number; env?: NodeJS.ProcessEnv } = {}) {
  return new Promise<{ code: number; stdout: string; stderr: string }>((resolve) => {
    execFile(
      /*turbopackIgnore: true*/ BIN,
      argv,
      { cwd: opts.cwd, timeout: opts.timeout ?? 120_000, maxBuffer: 64 << 20, env: opts.env },
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

/**
 * `rekcod host state NAME`: that Mac's engine, its actions rewritten to run there. rekcod gives
 * up after 20 s and reaps its ssh; past 45 s, or on `signal`, the whole process group goes.
 */
export async function readEngine(name: string, signal?: AbortSignal): Promise<State> {
  if (FIXTURE) return fakeEngine(await readState(), name)
  const r = await runGroup(BIN, ["host", "state", name], { timeout: 45_000, signal })
  if (r.code !== 0) throw new Error(`${name}: ${r.stderr.trim().split("\n").at(-1) || `exited ${r.code}`}`)
  return JSON.parse(r.stdout)
}

/** A fresh full state; the allow-list checks use this, never the cache. */
export const state = () => readState(false)

/**
 * The offered action with exactly this argv, or null. Only these run. With a host's engine
 * selected, its fresh state counts too.
 */
export async function offered(argv: unknown): Promise<Action | null> {
  const here = findOffered(await state(), argv)
  const name = engine()
  if (here || name === "local") return here
  return findOffered(await readEngine(name), argv)
}

/** Opens Terminal.app running the action's command line, as the menu bar does (server/terminal.ts). */
export function inTerminal(a: Action): string | null {
  const script = terminalScript(BIN, a.argv ?? [], a.dir)
  if (!script) return "the command has control characters"
  const as = appleScript(writeScript(script))
  if (!as) return "the script's path has control characters"
  spawn(/*turbopackIgnore: true*/ "/usr/bin/osascript", ["-e", as], { stdio: "ignore", detached: true }).unref()
  return null
}

export { STREAMED } from "./kinds"

export function spawnStream(a: Action) {
  return spawnRekcod(a.argv ?? [], a.dir)
}

export function spawnRekcod(argv: string[], cwd?: string, env?: NodeJS.ProcessEnv) {
  return spawn(/*turbopackIgnore: true*/ BIN, argv, { cwd, env, stdio: ["ignore", "pipe", "pipe"] })
}

/**
 * A background follower (`events`, `stats`). These are docker passthrough commands, which
 * boot a stopped VM; with REKCOD_NO_BOOT they exit instead, so a follower restarted on a
 * stale "running" state can't start a VM that was just stopped.
 */
export function spawnWatcher(argv: string[]) {
  return spawn(/*turbopackIgnore: true*/ BIN, argv, { env: watcherEnv(), stdio: ["ignore", "pipe", "pipe"] })
}

export const watcherEnv = (): NodeJS.ProcessEnv => ({ ...process.env, REKCOD_NO_BOOT: "1" })
