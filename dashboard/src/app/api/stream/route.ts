import { refuse } from "@/lib/guard"
import { FIXTURE, offered, spawnStream, STREAMED } from "@/lib/rekcod"
import { cappedStream } from "@/lib/server/stream"

// Streams a log-like action's output (`logs -f`, `app logs`, ...) as plain text until the
// client disconnects.
export async function GET(req: Request) {
  const no = refuse(req, false)
  if (no) return no
  let argv: unknown
  try {
    argv = JSON.parse(new URL(req.url).searchParams.get("argv") ?? "")
  } catch {
    return new Response("bad argv", { status: 400 })
  }
  const a = await offered(argv)
  if (!a || !STREAMED.has(a.kind)) return new Response("not a streamed action rekcod offers right now", { status: 400 })
  if (FIXTURE) return new Response(`dry run: rekcod ${a.argv!.join(" ")}\n`)
  return cappedStream(req, () => spawnStream(a))
}
