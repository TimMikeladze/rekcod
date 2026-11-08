import { refuse } from "@/lib/guard"
import { subscribe } from "@/lib/server/bus"
import { cached, get, start } from "@/lib/server/hub"
import { ensure } from "@/lib/server/sampler"
import { eventStream } from "@/lib/server/sse"

// Server-sent events: `state` (the whole document, after docker events and every 15 s),
// `stats` every 5 s, `activity` per action run, `fail` when `rekcod state` fails. The page
// reads it with fetch (lib/sse.ts), not EventSource, which can't send the token.
export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  const stream = eventStream(req.signal, (send, ping) => {
    const unsub = subscribe(send)
    start()
    ensure().catch(() => {})
    const now = cached()
    if (now) send("state", now)
    else get().catch((e) => send("fail", { error: String((e as Error).message) }))
    const timer = setInterval(ping, 20_000)
    return () => {
      unsub()
      clearInterval(timer)
    }
  })
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "cache-control": "no-store, no-transform", connection: "keep-alive" },
  })
}
