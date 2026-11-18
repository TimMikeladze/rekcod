import { describe, expect, it } from "vitest"
import { runGroup } from "./proc"

// Prints a grandchild's pid and waits, as `rekcod host state` does on a stalled ssh.
const PARENT = `
const { spawn } = require("node:child_process")
const c = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" })
console.log(c.pid)
setInterval(() => {}, 1000)
`

const alive = (pid: number) => {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

describe("runGroup", () => {
  it("kills what the command started when it times out", async () => {
    const r = await runGroup(process.execPath, ["-e", PARENT], { timeout: 500 })
    expect(r.code).toBe(1)
    expect(r.stderr).toContain("no answer in")
    const grandchild = Number(r.stdout.trim())
    expect(grandchild).toBeGreaterThan(0)
    await expect.poll(() => alive(grandchild), { timeout: 2000 }).toBe(false)
  })
  it("kills it when the caller gives up", async () => {
    const ctl = new AbortController()
    const p = runGroup(process.execPath, ["-e", PARENT], { timeout: 30_000, signal: ctl.signal })
    setTimeout(() => ctl.abort(), 300)
    const r = await p
    expect(r.stderr).toContain("cancelled")
    await expect.poll(() => alive(Number(r.stdout.trim())), { timeout: 2000 }).toBe(false)
  })
  it("returns the output of a command that finishes", async () => {
    expect(await runGroup(process.execPath, ["-e", "console.log('ok'); process.exit(3)"], { timeout: 5000 })).toEqual({ code: 3, stdout: "ok\n", stderr: "" })
  })
})
