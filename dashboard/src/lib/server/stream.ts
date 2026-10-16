import "server-only"
import type { ChildProcess } from "node:child_process"

/** A child's stdout and stderr as a plain-text response, killed when the client goes away. */
export function textStream(req: Request, child: ChildProcess): Response {
  const stream = new ReadableStream<Uint8Array>({
    start(ctl) {
      const push = (b: Buffer) => {
        try {
          ctl.enqueue(new Uint8Array(b))
        } catch {}
      }
      child.stdout?.on("data", push)
      child.stderr?.on("data", push)
      child.on("close", (code) => {
        push(Buffer.from(code ? `\n[exited ${code}]\n` : ""))
        try {
          ctl.close()
        } catch {}
      })
      req.signal.addEventListener("abort", () => child.kill())
    },
    cancel() {
      child.kill()
    },
  })
  return new Response(stream, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" } })
}

/** Fixed text, optionally dripped one line every `ms` (fixture mode's "follow"). */
export function fakeStream(req: Request, lines: string[], more?: () => string, ms = 1000): Response {
  let timer: ReturnType<typeof setInterval> | undefined
  const enc = new TextEncoder()
  const stream = new ReadableStream<Uint8Array>({
    start(ctl) {
      ctl.enqueue(enc.encode(lines.join("\n") + "\n"))
      if (!more) return ctl.close()
      timer = setInterval(() => {
        try {
          ctl.enqueue(enc.encode(more() + "\n"))
        } catch {
          clearInterval(timer)
        }
      }, ms)
      req.signal.addEventListener("abort", () => clearInterval(timer))
    },
    cancel() {
      clearInterval(timer)
    },
  })
  return new Response(stream, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" } })
}
