import { EventEmitter } from "node:events"
import { describe, expect, it, vi } from "vitest"

// `rekcod inspect` and `rekcod logs` are docker passthrough, which boots a stopped VM. The
// page opens them from a state that may be older than a `rekcod stop`.
const calls = vi.hoisted(() => ({ list: [] as { argv: string[]; env?: NodeJS.ProcessEnv }[] }))
vi.mock("node:child_process", async (orig) => ({
  ...(await orig<typeof import("node:child_process")>()),
  execFile: (_bin: string, argv: string[], opts: { env?: NodeJS.ProcessEnv }, cb: (e: null, out: string, err: string) => void) => {
    calls.list.push({ argv, env: opts.env })
    cb(null, "[{}]", "")
  },
  spawn: (_bin: string, argv: string[], opts: { env?: NodeJS.ProcessEnv }) => {
    calls.list.push({ argv, env: opts.env })
    return Object.assign(new EventEmitter(), { stdout: null, stderr: null, kill() {} })
  },
}))
vi.mock("@/lib/server/readonly", () => ({
  guarded: async (req: Request) => ({ s: { containers: [{ name: "web" }] }, q: new URL(req.url).searchParams, engine: "local" }),
  bad: (error: string) => Response.json({ error }, { status: 400 }),
}))

const inspect = await import("@/app/api/inspect/route")
const logs = await import("@/app/api/logs/route")

describe("container pages", () => {
  it("can't boot a stopped VM", async () => {
    await inspect.GET(new Request("http://127.0.0.1:4500/api/inspect?name=web"))
    await logs.GET(new Request("http://127.0.0.1:4500/api/logs?name=web&follow=1"))
    expect(calls.list.map((c) => c.argv[0])).toEqual(["inspect", "logs"])
    for (const c of calls.list) expect(c.env?.REKCOD_NO_BOOT).toBe("1")
  })
})
