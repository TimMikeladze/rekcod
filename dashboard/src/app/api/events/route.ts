import { refuse } from "@/lib/guard"
import { subscribe } from "@/lib/server/bus"
import { cached, get, start } from "@/lib/server/hub"
import { ensure } from "@/lib/server/sampler"

// Server-sent events: `state` (the whole document, after docker events and every 15 s),
// `stats` every 5 s, `activity` per action run, `fail` when `rekcod state` fails (not `error`: EventSource uses that name for its own).
export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  const enc = new TextEncoder()
  let unsub = () => {}
  let ping: ReturnType<typeof setInterval> | undefined
  const stream = new ReadableStream<Uint8Array>({
    start(ctl) {
      const send = (event: string, data: unknown) => ctl.enqueue(enc.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`))
      unsub = subscribe(send)
      start()
      ensure().catch(() => {})
      const now = cached()
      if (now) send("state", now)
      else get().catch((e) => send("fail", { error: String((e as Error).message) }))
      ping = setInterval(() => {
        try {
          ctl.enqueue(enc.encode(": ping\n\n"))
        } catch {
          clearInterval(ping)
        }
      }, 20_000)
      req.signal.addEventListener("abort", () => {
        unsub()
        clearInterval(ping)
        try {
          ctl.close()
        } catch {}
      })
    },
    cancel() {
      unsub()
      clearInterval(ping)
    },
  })
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "cache-control": "no-store, no-transform", connection: "keep-alive" },
  })
}
