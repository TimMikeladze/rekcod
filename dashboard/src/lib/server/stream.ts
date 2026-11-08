import "server-only"
import type { ChildProcess } from "node:child_process"
import type { Readable } from "node:stream"

// Each log route spawns a `rekcod` that lives as long as the request. Past this many, new
// ones get 429 instead of another process.
export const MAX_STREAMS = 16
let open = 0
export const openStreams = () => open

/**
 * Bytes a log stream queues for a client that stops reading. Past it the child's pipes are
 * paused until the client reads again, so the child blocks on its writes and the server holds
 * at most this plus a pipe chunk per output.
 */
export const HIGH_WATER = 64 * 1024

/** `start()`'s child as a text stream, or 429 when MAX_STREAMS of them are running. */
export function cappedStream(req: Request, start: () => ChildProcess): Response {
  if (open >= MAX_STREAMS) return new Response(`${MAX_STREAMS} streams are already open; close one`, { status: 429 })
  const child = start()
  open++
  let done = false
  const release = () => {
    if (!done) open--
    done = true
  }
  child.once("close", release)
  child.once("error", release)
  return textStream(req, child)
}

/** A child's stdout and stderr as a plain-text response, killed when the client goes away. */
export function textStream(req: Request, child: ChildProcess): Response {
  const outs = [child.stdout, child.stderr].filter((s): s is Readable => !!s)
  let closed = false
  const stream = new ReadableStream<Uint8Array>(
    {
      start(ctl) {
        const end = (note: string) => {
          if (closed) return
          closed = true
          if (note) ctl.enqueue(new TextEncoder().encode(note))
          ctl.close()
        }
        for (const s of outs)
          s.on("data", (b: Buffer) => {
            if (closed) return
            ctl.enqueue(new Uint8Array(b))
            if ((ctl.desiredSize ?? 0) <= 0) for (const o of outs) o.pause()
          })
        child.once("close", (code) => end(code ? `\n[exited ${code}]\n` : ""))
        child.once("error", (e) => end(`\n[${e.message}]\n`))
        req.signal.addEventListener("abort", () => child.kill())
      },
      // Runs when the client has read the queue down below HIGH_WATER.
      pull() {
        for (const o of outs) o.resume()
      },
      cancel() {
        closed = true
        child.kill()
      },
    },
    new ByteLengthQueuingStrategy({ highWaterMark: HIGH_WATER }),
  )
  return new Response(stream, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" } })
}

/** Fixed text, optionally dripped one line every `ms` (fixture mode's "follow"). A line the client has no room for is dropped. */
export function fakeStream(req: Request, lines: string[], more?: () => string, ms = 1000): Response {
  let timer: ReturnType<typeof setInterval> | undefined
  const enc = new TextEncoder()
  const stream = new ReadableStream<Uint8Array>(
    {
      start(ctl) {
        ctl.enqueue(enc.encode(lines.join("\n") + "\n"))
        if (!more) return ctl.close()
        timer = setInterval(() => {
          try {
            if ((ctl.desiredSize ?? 0) > 0) ctl.enqueue(enc.encode(more() + "\n"))
          } catch {
            clearInterval(timer)
          }
        }, ms)
        req.signal.addEventListener("abort", () => clearInterval(timer))
      },
      cancel() {
        clearInterval(timer)
      },
    },
    new ByteLengthQueuingStrategy({ highWaterMark: HIGH_WATER }),
  )
  return new Response(stream, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" } })
}
