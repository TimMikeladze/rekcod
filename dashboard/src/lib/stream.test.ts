import { EventEmitter } from "node:events"
import { PassThrough } from "node:stream"
import type { ChildProcess } from "node:child_process"
import { describe, expect, it, vi } from "vitest"
import { eventStream, HIGH_WATER as SSE_HIGH_WATER, MAX_PENDING } from "./server/sse"
import { cappedStream, HIGH_WATER, MAX_STREAMS, openStreams, textStream } from "./server/stream"
import { readEvents } from "./sse"

type FakeChild = EventEmitter & { stdout: PassThrough; stderr: PassThrough; kill: () => void }

function fakeChild(): FakeChild {
  const c = new EventEmitter() as FakeChild
  c.stdout = new PassThrough()
  c.stderr = new PassThrough()
  c.kill = vi.fn(() => c.emit("close", null))
  return c
}
const asChild = (c: FakeChild) => c as unknown as ChildProcess
const req = () => new Request("http://localhost:4500/api/logs")

describe("stream cap", () => {
  it(`spawns up to ${MAX_STREAMS}, then 429s without spawning until one closes`, () => {
    const kids: FakeChild[] = []
    const start = () => {
      const c = fakeChild()
      kids.push(c)
      return asChild(c)
    }
    for (let i = 0; i < MAX_STREAMS; i++) expect(cappedStream(req(), start).status).toBe(200)
    expect(openStreams()).toBe(MAX_STREAMS)
    expect(cappedStream(req(), start).status).toBe(429)
    expect(kids).toHaveLength(MAX_STREAMS)

    // close and error on the same child count once.
    kids[0].emit("error", new Error("spawn failed"))
    kids[0].emit("close", 1)
    expect(openStreams()).toBe(MAX_STREAMS - 1)
    expect(cappedStream(req(), start).status).toBe(200)
    for (const k of kids) k.emit("close", 0)
    expect(openStreams()).toBe(0)
  })
})

describe("log stream backpressure", () => {
  it("pauses a noisy child while the reader stalls, holds a bounded amount, and delivers it all later", async () => {
    const c = fakeChild()
    const reader = textStream(req(), asChild(c)).body!.getReader()
    const TOTAL = 16 * 1024 * 1024
    const chunk = Buffer.alloc(16 * 1024, 0x78)
    let written = 0
    let blocked = false
    // Writes like a child into a pipe: when the pipe is full it waits for drain.
    const produce = () => {
      while (written < TOTAL) {
        written += chunk.length
        if (!c.stdout.write(chunk)) {
          blocked = true
          c.stdout.once("drain", () => {
            blocked = false
            produce()
          })
          return
        }
      }
    }
    produce()
    await vi.waitFor(() => expect(blocked).toBe(true))
    await new Promise((r) => setTimeout(r, 50))
    expect(c.stdout.readableFlowing).toBe(false)
    expect(blocked).toBe(true)
    // Everything written so far sits in the response queue or the pipe's own small buffers.
    expect(written).toBeLessThan(HIGH_WATER + 128 * 1024)

    let got = 0
    while (got < TOTAL) {
      const { done, value } = await reader.read()
      if (done) break
      got += value.byteLength
    }
    expect(got).toBe(TOTAL)
    await reader.cancel()
    expect(c.kill).toHaveBeenCalled()
  })

  it("ends with the exit code when the child fails", async () => {
    const c = fakeChild()
    const body = textStream(req(), asChild(c)).body!
    c.stderr.write("boom\n")
    await new Promise((r) => setTimeout(r, 0))
    c.emit("close", 3)
    expect(await new Response(body).text()).toBe("boom\n\n[exited 3]\n")
  })
})

describe("event stream backpressure", () => {
  const big = "x".repeat(100 * 1024)

  it("keeps only the newest state for a stalled page, and each activity entry's newest", async () => {
    let send!: (event: string, data: unknown) => void
    const teardown = vi.fn()
    const ac = new AbortController()
    const body = eventStream(ac.signal, (s) => {
      send = s
      return teardown
    })
    for (let i = 0; i < 1000; i++) send("state", { i, big })
    send("activity", { id: 1 })
    send("activity", { id: 1, code: 0 })
    send("activity", { id: 2 })

    const got: { event: string; data: Record<string, unknown> }[] = []
    let bytes = 0
    const reading = readEvents(body, (event, data) => {
      bytes += data.length
      got.push({ event, data: JSON.parse(data) })
    })
    await vi.waitFor(() => expect(got.filter((e) => e.event === "activity")).toHaveLength(2))
    ac.abort()
    await reading

    const states = got.filter((e) => e.event === "state").map((e) => e.data.i)
    expect(states.at(-1)).toBe(999)
    expect(states.length).toBeLessThanOrEqual(Math.ceil(SSE_HIGH_WATER / big.length) + 1)
    expect(bytes).toBeLessThan(SSE_HIGH_WATER + 2 * big.length + 1024)
    expect(got.filter((e) => e.event === "activity").map((e) => e.data)).toEqual([{ id: 1, code: 0 }, { id: 2 }])
    expect(teardown).toHaveBeenCalledTimes(1)
  })

  it("drops a page that falls more than MAX_PENDING frames behind", async () => {
    let send!: (event: string, data: unknown) => void
    const teardown = vi.fn()
    const body = eventStream(new AbortController().signal, (s) => {
      send = s
      return teardown
    })
    for (let i = 0; i < 3; i++) send("state", { i, big })
    for (let id = 0; id <= MAX_PENDING; id++) send("activity", { id })
    expect(teardown).toHaveBeenCalledTimes(1)
    send("state", { i: 3 })
    await expect(readEvents(body, () => {})).rejects.toThrow(/fell behind/)
  })
})

describe("readEvents", () => {
  it("parses events split across chunks, mid-line and mid-character", async () => {
    const bytes = new TextEncoder().encode('event: state\ndata: {"a":1}\n\n: ping\n\nevent: x\r\ndata: one\r\ndata: two\r\n\r\ndata: é\n\n')
    const cuts = [5, 20, 33, bytes.length - 3, bytes.length]
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        let from = 0
        for (const to of cuts) {
          c.enqueue(bytes.slice(from, to))
          from = to
        }
        c.close()
      },
    })
    const got: [string, string][] = []
    await readEvents(body, (e, d) => got.push([e, d]))
    expect(got).toEqual([
      ["state", '{"a":1}'],
      ["x", "one\ntwo"],
      ["message", "é"],
    ])
  })
})
