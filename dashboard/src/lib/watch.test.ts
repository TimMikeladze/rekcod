import { describe, expect, it, vi } from "vitest"

const { spawn } = vi.hoisted(() => ({ spawn: vi.fn(() => ({})) }))
vi.mock("node:child_process", async (orig) => ({ ...(await orig<typeof import("node:child_process")>()), spawn }))

import { spawnRekcod, spawnWatcher } from "./rekcod"

describe("background followers", () => {
  it("can't boot a stopped VM", () => {
    spawnWatcher(["events", "--format", "{{json .}}"])
    const [, argv, opts] = spawn.mock.calls.at(-1) as unknown as [string, string[], { env: NodeJS.ProcessEnv }]
    expect(argv).toEqual(["events", "--format", "{{json .}}"])
    expect(opts.env.REKCOD_NO_BOOT).toBe("1")
    expect(opts.env.PATH).toBe(process.env.PATH)
  })
  it("leaves actions free to boot it", () => {
    spawnRekcod(["start"])
    const [, , opts] = spawn.mock.calls.at(-1) as unknown as [string, string[], { env?: NodeJS.ProcessEnv }]
    expect(opts.env).toBeUndefined()
  })
})
