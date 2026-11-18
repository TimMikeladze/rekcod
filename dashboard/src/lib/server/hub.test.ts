import { afterEach, describe, expect, it, vi } from "vitest"
import type { State } from "@/lib/types"

// A host's read that only settles when the test says so.
const reads = vi.hoisted(() => ({ engine: [] as { name: string; resolve: (s: State) => void; signal?: AbortSignal }[] }))

vi.mock("@/lib/rekcod", () => ({
  FIXTURE: "",
  readState: async () => ({ running: true, containers: [], hosts: { all: [{ name: "mini" }] } }) as unknown as State,
  readEngine: (name: string, signal?: AbortSignal) =>
    new Promise<State>((resolve) => reads.engine.push({ name, resolve, signal })),
  spawnWatcher: () => ({ stdout: null, on() {}, kill() {} }),
}))

const { cached, get, refreshFull, switchEngine } = await import("./hub")
const { setEngine } = await import("./engine")

afterEach(() => {
  reads.engine.length = 0
})

describe("switching engines", () => {
  it("drops the old engine's read and kills it", async () => {
    setEngine("mini")
    const slow = refreshFull()
    await vi.waitFor(() => expect(reads.engine).toHaveLength(1))
    const back = switchEngine("local")
    expect(reads.engine[0].signal?.aborted).toBe(true)
    // The host answers after the switch: nobody gets its state.
    reads.engine[0].resolve({ running: true, containers: [{ name: "theirs" }], engine: "mini" } as unknown as State)
    const [a, b] = await Promise.all([slow, back])
    expect(a.engine).toBeUndefined()
    expect(b.engine).toBeUndefined()
    expect(cached()?.engine).toBeUndefined()
  })
})

describe("an idle hub", () => {
  it("drops the state nobody refreshes, so a page opened later reads a fresh one", async () => {
    vi.useFakeTimers()
    try {
      setEngine("local")
      await get()
      expect(cached()).not.toBeNull()
      await vi.advanceTimersByTimeAsync(61_000)
      expect(cached()).toBeNull()
    } finally {
      vi.useRealTimers()
    }
  })
})
